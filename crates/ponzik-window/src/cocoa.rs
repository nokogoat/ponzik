//! Fenêtre Cocoa : on parle au runtime Objective-C directement (`objc_msgSend`,
//! `objc_getClass`, `sel_registerName`), sans crate `objc` ni `cocoa`.
//!
//! Deux classes sont créées à l'exécution : `PonzikView` (sous-classe de `NSView`)
//! reçoit souris et clavier, `PonzikWindowDelegate` les fermetures, redimensionnements
//! et pertes de focus. Les événements sont rangés dans une file locale au thread
//! principal, vidée par `poll_events` / `wait_events` qui pompent eux-mêmes la file
//! de `NSApplication` (pas de `[NSApp run]`).
//!
//! L'image est posée telle quelle comme contenu (`CGImage`) du calque de la vue.
//! Écrans Retina : par défaut on travaille en points (taille logique) et le
//! compositeur double les pixels, car l'interface n'a pas encore de facteur
//! d'échelle ; `PONZIK_RETINA=1` rend à la résolution native de l'écran
//! (interface deux fois plus petite, mais nette).

#![allow(non_upper_case_globals, clippy::upper_case_acronyms)]

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::{null, null_mut};
use std::time::Duration;

use crate::{Cursor, Event, Key, Modifiers, MouseButton, WindowError};

type Id = *mut c_void;
type Sel = *const c_void;
type Class = *mut c_void;
/// `BOOL` : `bool` sur arm64, `signed char` sur x86_64 ; un octet dans les deux cas.
type Bool = i8;
const YES: Bool = 1;
const NO: Bool = 0;
type CFStringRef = *const c_void;
type CGColorSpaceRef = *mut c_void;
type CGDataProviderRef = *mut c_void;
type CGImageRef = *mut c_void;
type CFDataRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Class;
    fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra: usize) -> Class;
    fn objc_registerClassPair(class: Class);
    fn class_addMethod(class: Class, sel: Sel, imp: *const c_void, types: *const c_char) -> Bool;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSDefaultRunLoopMode: Id;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: CFStringRef;
    fn CGColorSpaceCreateWithName(name: CFStringRef) -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGDataProviderCreateWithCFData(data: CFDataRef) -> CGDataProviderRef;
    fn CGDataProviderRelease(provider: CGDataProviderRef);
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
        provider: CGDataProviderRef,
        decode: *const f64,
        should_interpolate: bool,
        intent: i32,
    ) -> CGImageRef;
    fn CGImageRelease(image: CGImageRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(alloc: *const c_void, bytes: *const u8, len: isize) -> CFDataRef;
    fn CFRelease(object: *const c_void);
}

const NSWindowStyleMaskTitled: usize = 1 << 0;
const NSWindowStyleMaskClosable: usize = 1 << 1;
const NSWindowStyleMaskMiniaturizable: usize = 1 << 2;
const NSWindowStyleMaskResizable: usize = 1 << 3;
const NSBackingStoreBuffered: usize = 2;
const NSApplicationActivationPolicyRegular: isize = 0;
const NSEventMaskAny: u64 = u64::MAX;
const NSEventTypeKeyUp: usize = 11;

const NSEventModifierFlagShift: usize = 1 << 17;
const NSEventModifierFlagControl: usize = 1 << 18;
const NSEventModifierFlagOption: usize = 1 << 19;
const NSEventModifierFlagCommand: usize = 1 << 20;
/// Bits « dépendants du périphérique » (IOKit) : distinguent gauche et droite.
const NX_DEVICELCTLKEYMASK: usize = 0x0001;
const NX_DEVICELSHIFTKEYMASK: usize = 0x0002;
const NX_DEVICERSHIFTKEYMASK: usize = 0x0004;
const NX_DEVICELCMDKEYMASK: usize = 0x0008;
const NX_DEVICERCMDKEYMASK: usize = 0x0010;
const NX_DEVICELALTKEYMASK: usize = 0x0020;
const NX_DEVICERALTKEYMASK: usize = 0x0040;
const NX_DEVICERCTLKEYMASK: usize = 0x2000;
const NSEventModifierFlagCapsLock: usize = 1 << 16;

const NSTrackingMouseEnteredAndExited: usize = 0x01;
const NSTrackingMouseMoved: usize = 0x02;
const NSTrackingCursorUpdate: usize = 0x04;
const NSTrackingActiveAlways: usize = 0x80;
const NSTrackingInVisibleRect: usize = 0x200;

/// `kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst` : un mot 32 bits
/// natif `0x00RRGGBB`, soit exactement notre tampon.
const BITMAP_INFO: u32 = (2 << 12) | 6;

const MIN_SIZE: (f64, f64) = (960.0, 640.0);

// ---------------------------------------------------------------- runtime Objective-C

fn sel(name: &CStr) -> Sel {
    // SAFETY: nom de sélecteur terminé par zéro.
    unsafe { sel_registerName(name.as_ptr()) }
}

fn class(name: &CStr) -> Class {
    // SAFETY: nom de classe terminé par zéro ; nul si la classe n'existe pas.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// Arguments d'un message : chaque arité appelle `objc_msgSend` avec la signature exacte
/// (obligatoire sur arm64, où la fonction n'est pas variadique).
trait Args {
    unsafe fn send<R>(self, obj: Id, sel: Sel) -> R;
}

macro_rules! impl_args {
    ($($a:ident),*) => {
        impl<$($a),*> Args for ($($a,)*) {
            #[allow(non_snake_case)]
            unsafe fn send<R>(self, obj: Id, sel: Sel) -> R {
                let ($($a,)*) = self;
                // SAFETY: l'appelant garantit que le sélecteur attend ces arguments et ce retour.
                unsafe {
                    let f: unsafe extern "C" fn(Id, Sel $(, $a)*) -> R =
                        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                    f(obj, sel $(, $a)*)
                }
            }
        }
    };
}

impl_args!();
impl_args!(A);
impl_args!(A, B);
impl_args!(A, B, C);
impl_args!(A, B, C, D);

/// Envoie `name` à `obj`. L'appelant choisit des types d'arguments et de retour
/// conformes à la déclaration Objective-C du sélecteur.
unsafe fn msg<R>(obj: Id, name: &CStr, args: impl Args) -> R {
    // SAFETY: reporté sur l'appelant.
    unsafe { args.send(obj, sel(name)) }
}

/// Message renvoyant un `NSRect` : sur x86_64 la structure revient par la mémoire.
unsafe fn msg_rect(obj: Id, name: &CStr) -> NSRect {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: signature `objc_msgSend_stret(NSRect*, id, SEL)`.
    unsafe {
        let mut out = NSRect::default();
        let f: unsafe extern "C" fn(*mut NSRect, Id, Sel) =
            std::mem::transmute(objc_msgSend_stret as unsafe extern "C" fn());
        f(&mut out, obj, sel(name));
        out
    }
    #[cfg(not(target_arch = "x86_64"))]
    // SAFETY: sur arm64, un NSRect (quatre doubles) revient dans les registres.
    unsafe {
        msg(obj, name, ())
    }
}

/// Bassin d'autolibération : tout objet « autoreleased » créé pendant sa vie est libéré à la fin.
struct Pool(*mut c_void);

impl Pool {
    fn new() -> Self {
        // SAFETY: appariement strict avec le Pop de Drop.
        Pool(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: jeton rendu par objc_autoreleasePoolPush sur ce thread.
        unsafe { objc_autoreleasePoolPop(self.0) };
    }
}

/// `NSString` autolibérée (à utiliser sous un `Pool`).
fn ns_string(s: &str) -> Id {
    let c = CString::new(s.replace('\0', " ")).unwrap_or_default();
    // SAFETY: chaîne UTF-8 terminée par zéro.
    unsafe { msg(class(c"NSString"), c"stringWithUTF8String:", (c.as_ptr(),)) }
}

fn rust_string(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: `s` est une NSString ; UTF8String vit autant que le bassin courant.
    unsafe {
        let p: *const c_char = msg(s, c"UTF8String", ());
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

// ---------------------------------------------------------------- état partagé avec les rappels

thread_local! {
    static QUEUE: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    /// Pixels de tampon par point : 1, ou le facteur de l'écran avec `PONZIK_RETINA=1`.
    static SCALE: Cell<f64> = const { Cell::new(1.0) };
    static RETINA: Cell<bool> = const { Cell::new(false) };
    static CURSOR: Cell<Cursor> = const { Cell::new(Cursor::Arrow) };
    /// La souris survole la zone cliente.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

fn push(ev: Event) {
    QUEUE.with(|q| q.borrow_mut().push(ev));
}

fn modifiers(flags: usize) -> Modifiers {
    Modifiers {
        shift: flags & NSEventModifierFlagShift != 0,
        // Cmd fait office de Ctrl : les raccourcis Ctrl+… deviennent Cmd+… comme partout sur Mac.
        ctrl: flags & (NSEventModifierFlagCommand | NSEventModifierFlagControl) != 0,
        alt: flags & NSEventModifierFlagOption != 0,
    }
}

/// Taille de la vue, en pixels de tampon.
fn view_size(view: Id) -> (u32, u32) {
    // SAFETY: `view` est une NSView vivante.
    let bounds = unsafe { msg_rect(view, c"bounds") };
    let s = SCALE.get();
    ((bounds.size.width * s).round().max(1.0) as u32, (bounds.size.height * s).round().max(1.0) as u32)
}

/// Relit le facteur d'échelle de la fenêtre (changement d'écran).
fn update_scale(window: Id, view: Id) {
    // SAFETY: objets vivants ; `layer` existe dès que la vue a un calque.
    unsafe {
        let backing: f64 = msg(window, c"backingScaleFactor", ());
        let backing = if backing > 0.0 { backing } else { 1.0 };
        let scale = if RETINA.get() { backing } else { 1.0 };
        SCALE.set(scale);
        let layer: Id = msg(view, c"layer", ());
        if !layer.is_null() {
            let () = msg(layer, c"setContentsScale:", (scale,));
        }
    }
}

/// Position de la souris dans la vue, en pixels de tampon (origine en haut à gauche).
fn mouse_pos(view: Id, event: Id) -> (f32, f32) {
    // SAFETY: vue vivante, événement souris.
    let p: NSPoint = unsafe {
        let p: NSPoint = msg(event, c"locationInWindow", ());
        msg(view, c"convertPoint:fromView:", (p, null_mut::<c_void>()))
    };
    let s = SCALE.get();
    ((p.x * s) as f32, (p.y * s) as f32)
}

fn event_flags(event: Id) -> usize {
    // SAFETY: tout NSEvent répond à modifierFlags.
    unsafe { msg(event, c"modifierFlags", ()) }
}

fn ns_cursor(cursor: Cursor) -> Id {
    let ns = class(c"NSCursor");
    let get = |name: &CStr| -> Id {
        // SAFETY: méthode de classe sans argument ; on vérifie qu'elle existe (curseurs privés).
        unsafe {
            let ok: Bool = msg(ns, c"respondsToSelector:", (sel(name),));
            if ok != NO { msg(ns, name, ()) } else { null_mut() }
        }
    };
    let id = match cursor {
        Cursor::Arrow => get(c"arrowCursor"),
        Cursor::Hand => get(c"pointingHandCursor"),
        Cursor::Text => get(c"IBeamCursor"),
        Cursor::ResizeH => get(c"resizeLeftRightCursor"),
        Cursor::ResizeV => get(c"resizeUpDownCursor"),
        // Pas de curseur public en diagonale : celui du coin de fenêtre, s'il existe.
        Cursor::ResizeNwSe => get(c"_windowResizeNorthWestSouthEastCursor"),
        Cursor::Move => get(c"openHandCursor"),
        Cursor::Grab => get(c"closedHandCursor"),
        Cursor::Crosshair => get(c"crosshairCursor"),
        Cursor::NotAllowed => get(c"operationNotAllowedCursor"),
    };
    if id.is_null() { get(c"arrowCursor") } else { id }
}

fn apply_cursor() {
    let _pool = Pool::new();
    let cursor = ns_cursor(CURSOR.get());
    if !cursor.is_null() {
        // SAFETY: curseur système partagé.
        unsafe {
            let () = msg(cursor, c"set", ());
        }
    }
}

// ---------------------------------------------------------------- clavier

/// Touche logique d'après le code virtuel Mac (`kVK_*`), hors caractères.
fn special_key(code: u16) -> Option<Key> {
    Some(match code {
        0x31 => Key::Space,
        0x24 | 0x4C => Key::Enter,
        0x35 => Key::Escape,
        0x33 => Key::Backspace,
        0x30 => Key::Tab,
        0x75 => Key::Delete,
        0x7B => Key::Left,
        0x7C => Key::Right,
        0x7D => Key::Down,
        0x7E => Key::Up,
        0x7A => Key::F(1),
        0x78 => Key::F(2),
        0x63 => Key::F(3),
        0x76 => Key::F(4),
        0x60 => Key::F(5),
        0x61 => Key::F(6),
        0x62 => Key::F(7),
        0x64 => Key::F(8),
        0x65 => Key::F(9),
        0x6D => Key::F(10),
        0x67 => Key::F(11),
        0x6F => Key::F(12),
        0x74 => Key::PageUp,
        0x79 => Key::PageDown,
        0x73 => Key::Home,
        0x77 => Key::End,
        0x45 => Key::Char('+'),
        0x4E => Key::Char('-'),
        _ => return None,
    })
}

/// Caractère imprimable utilisable comme touche (hors zone privée des touches de fonction).
fn printable(c: char) -> bool {
    !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c)
}

/// Touche d'un événement clavier : spéciale par position, sinon le caractère
/// de la disposition sans modificateur (comme la colonne 0 de X11), en minuscule.
fn event_key(event: Id, code: u16) -> Option<Key> {
    if let Some(key) = special_key(code) {
        return Some(key);
    }
    // SAFETY: événement clavier ; charactersByApplyingModifiers: (10.15+) vérifié avant usage.
    let chars = unsafe {
        let ok: Bool = msg(event, c"respondsToSelector:", (sel(c"charactersByApplyingModifiers:"),));
        if ok != NO {
            msg(event, c"charactersByApplyingModifiers:", (0usize,))
        } else {
            msg(event, c"charactersIgnoringModifiers", ())
        }
    };
    let c = rust_string(chars).chars().next().filter(|&c| printable(c))?;
    Some(Key::Char(c.to_lowercase().next()?))
}

/// Code evdev (Linux) de la touche à cette position : même convention que X11 et Windows.
fn evdev_code(code: u16) -> Option<u16> {
    Some(match code {
        0x00 => 30,  // A
        0x01 => 31,  // S
        0x02 => 32,  // D
        0x03 => 33,  // F
        0x04 => 35,  // H
        0x05 => 34,  // G
        0x06 => 44,  // Z
        0x07 => 45,  // X
        0x08 => 46,  // C
        0x09 => 47,  // V
        0x0A => 86,  // ISO § (102e touche)
        0x0B => 48,  // B
        0x0C => 16,  // Q
        0x0D => 17,  // W
        0x0E => 18,  // E
        0x0F => 19,  // R
        0x10 => 21,  // Y
        0x11 => 20,  // T
        0x12 => 2,   // 1
        0x13 => 3,   // 2
        0x14 => 4,   // 3
        0x15 => 5,   // 4
        0x16 => 7,   // 6
        0x17 => 6,   // 5
        0x18 => 13,  // =
        0x19 => 10,  // 9
        0x1A => 8,   // 7
        0x1B => 12,  // -
        0x1C => 9,   // 8
        0x1D => 11,  // 0
        0x1E => 27,  // ]
        0x1F => 24,  // O
        0x20 => 22,  // U
        0x21 => 26,  // [
        0x22 => 23,  // I
        0x23 => 25,  // P
        0x24 => 28,  // Entrée
        0x25 => 38,  // L
        0x26 => 36,  // J
        0x27 => 40,  // '
        0x28 => 37,  // K
        0x29 => 39,  // ;
        0x2A => 43,  // \
        0x2B => 51,  // ,
        0x2C => 53,  // /
        0x2D => 49,  // N
        0x2E => 50,  // M
        0x2F => 52,  // .
        0x30 => 15,  // Tab
        0x31 => 57,  // Espace
        0x32 => 41,  // `
        0x33 => 14,  // Retour arrière
        0x35 => 1,   // Échap
        0x36 => 126, // Cmd droite
        0x37 => 125, // Cmd gauche
        0x38 => 42,  // Maj gauche
        0x39 => 58,  // Verr. Maj
        0x3A => 56,  // Option gauche
        0x3B => 29,  // Ctrl gauche
        0x3C => 54,  // Maj droite
        0x3D => 100, // Option droite
        0x3E => 97,  // Ctrl droite
        0x40 => 187, // F17
        0x41 => 83,  // pavé .
        0x43 => 55,  // pavé *
        0x45 => 78,  // pavé +
        0x47 => 69,  // pavé Clear (Verr. Num)
        0x48 => 115, // volume +
        0x49 => 114, // volume -
        0x4A => 113, // muet
        0x4B => 98,  // pavé /
        0x4C => 96,  // pavé Entrée
        0x4E => 74,  // pavé -
        0x4F => 188, // F18
        0x50 => 189, // F19
        0x51 => 117, // pavé =
        0x52 => 82,  // pavé 0
        0x53 => 79,  // pavé 1
        0x54 => 80,  // pavé 2
        0x55 => 81,  // pavé 3
        0x56 => 75,  // pavé 4
        0x57 => 76,  // pavé 5
        0x58 => 77,  // pavé 6
        0x59 => 71,  // pavé 7
        0x5A => 190, // F20
        0x5B => 72,  // pavé 8
        0x5C => 73,  // pavé 9
        0x5D => 124, // JIS ¥
        0x5E => 89,  // JIS _
        0x5F => 95,  // JIS pavé ,
        0x60 => 63,  // F5
        0x61 => 64,  // F6
        0x62 => 65,  // F7
        0x63 => 61,  // F3
        0x64 => 66,  // F8
        0x65 => 67,  // F9
        0x67 => 87,  // F11
        0x69 => 183, // F13
        0x6A => 186, // F16
        0x6B => 184, // F14
        0x6D => 68,  // F10
        0x6F => 88,  // F12
        0x71 => 185, // F15
        0x72 => 110, // Help (Insert sur PC)
        0x73 => 102, // Début
        0x74 => 104, // Page préc.
        0x75 => 111, // Suppr
        0x76 => 62,  // F4
        0x77 => 107, // Fin
        0x78 => 60,  // F2
        0x79 => 109, // Page suiv.
        0x7A => 59,  // F1
        0x7B => 105, // Gauche
        0x7C => 106, // Droite
        0x7D => 108, // Bas
        0x7E => 103, // Haut
        _ => return None,
    })
}

/// Pour une touche de modification, le bit qui dit si elle est enfoncée.
fn modifier_mask(code: u16) -> Option<usize> {
    Some(match code {
        0x38 => NX_DEVICELSHIFTKEYMASK,
        0x3C => NX_DEVICERSHIFTKEYMASK,
        0x3B => NX_DEVICELCTLKEYMASK,
        0x3E => NX_DEVICERCTLKEYMASK,
        0x3A => NX_DEVICELALTKEYMASK,
        0x3D => NX_DEVICERALTKEYMASK,
        0x37 => NX_DEVICELCMDKEYMASK,
        0x36 => NX_DEVICERCMDKEYMASK,
        0x39 => NSEventModifierFlagCapsLock,
        _ => return None,
    })
}

// ---------------------------------------------------------------- méthodes de PonzikView

extern "C" fn yes(_: Id, _: Sel) -> Bool {
    YES
}

extern "C" fn yes_with(_: Id, _: Sel, _: Id) -> Bool {
    YES
}

/// `updateLayer` : appelé à la place de `drawRect:` ; on ne fait que demander une image.
extern "C" fn update_layer(_: Id, _: Sel) {
    push(Event::Redraw);
}

fn mouse_down(view: Id, event: Id, button: MouseButton) {
    let (x, y) = mouse_pos(view, event);
    push(Event::MouseDown { x, y, button, mods: modifiers(event_flags(event)) });
}

fn mouse_up(view: Id, event: Id, button: MouseButton) {
    let (x, y) = mouse_pos(view, event);
    push(Event::MouseUp { x, y, button });
}

/// Bouton « autre » : 2 est le bouton du milieu, les suivants sont ignorés.
fn other_button(event: Id) -> Option<MouseButton> {
    // SAFETY: événement souris.
    let n: isize = unsafe { msg(event, c"buttonNumber", ()) };
    (n == 2).then_some(MouseButton::Middle)
}

extern "C" fn on_mouse_down(this: Id, _: Sel, event: Id) {
    mouse_down(this, event, MouseButton::Left);
}

extern "C" fn on_mouse_up(this: Id, _: Sel, event: Id) {
    mouse_up(this, event, MouseButton::Left);
}

extern "C" fn on_right_mouse_down(this: Id, _: Sel, event: Id) {
    mouse_down(this, event, MouseButton::Right);
}

extern "C" fn on_right_mouse_up(this: Id, _: Sel, event: Id) {
    mouse_up(this, event, MouseButton::Right);
}

extern "C" fn on_other_mouse_down(this: Id, _: Sel, event: Id) {
    if let Some(button) = other_button(event) {
        mouse_down(this, event, button);
    }
}

extern "C" fn on_other_mouse_up(this: Id, _: Sel, event: Id) {
    if let Some(button) = other_button(event) {
        mouse_up(this, event, button);
    }
}

extern "C" fn on_mouse_moved(this: Id, _: Sel, event: Id) {
    let (x, y) = mouse_pos(this, event);
    push(Event::MouseMove { x, y });
}

extern "C" fn on_mouse_entered(_: Id, _: Sel, _: Id) {
    INSIDE.set(true);
    apply_cursor();
}

extern "C" fn on_mouse_exited(_: Id, _: Sel, _: Id) {
    INSIDE.set(false);
    push(Event::MouseLeave);
}

extern "C" fn on_cursor_update(_: Id, _: Sel, _: Id) {
    apply_cursor();
}

extern "C" fn on_scroll_wheel(this: Id, _: Sel, event: Id) {
    // SAFETY: événement de défilement.
    let (dy, precise) = unsafe {
        let dy: f64 = msg(event, c"scrollingDeltaY", ());
        let precise: Bool = msg(event, c"hasPreciseScrollingDeltas", ());
        (dy, precise != NO)
    };
    // Pavé tactile : des points, environ douze par cran de molette.
    let delta = if precise { dy / 12.0 } else { dy } as f32;
    if delta == 0.0 {
        return;
    }
    let (x, y) = mouse_pos(this, event);
    push(Event::Wheel { x, y, delta, mods: modifiers(event_flags(event)) });
}

extern "C" fn on_key_down(_: Id, _: Sel, event: Id) {
    // SAFETY: événement clavier.
    let (code, repeat, chars): (u16, Bool, Id) =
        unsafe { (msg(event, c"keyCode", ()), msg(event, c"isARepeat", ()), msg(event, c"characters", ())) };
    let flags = event_flags(event);
    if repeat == NO
        && let Some(evdev) = evdev_code(code)
    {
        push(Event::Physical { code: evdev, down: true });
    }
    if let Some(key) = event_key(event, code) {
        push(Event::KeyDown { key, mods: modifiers(flags) });
    }
    // Saisie de texte, sauf quand Cmd ou Ctrl font un raccourci (Option compose des caractères).
    if flags & (NSEventModifierFlagCommand | NSEventModifierFlagControl) == 0 {
        for c in rust_string(chars).chars().filter(|&c| printable(c)) {
            push(Event::Text(c));
        }
    }
}

extern "C" fn on_key_up(_: Id, _: Sel, event: Id) {
    // SAFETY: événement clavier.
    let code: u16 = unsafe { msg(event, c"keyCode", ()) };
    if let Some(evdev) = evdev_code(code) {
        push(Event::Physical { code: evdev, down: false });
    }
    if let Some(key) = event_key(event, code) {
        push(Event::KeyUp { key });
    }
}

/// Touches de modification : Cocoa n'envoie qu'un changement d'état, on en déduit le sens.
extern "C" fn on_flags_changed(_: Id, _: Sel, event: Id) {
    // SAFETY: événement clavier.
    let code: u16 = unsafe { msg(event, c"keyCode", ()) };
    if let (Some(evdev), Some(mask)) = (evdev_code(code), modifier_mask(code)) {
        push(Event::Physical { code: evdev, down: event_flags(event) & mask != 0 });
    }
}

// ---------------------------------------------------------------- méthodes du délégué

fn delegate_view(notification: Id) -> Option<(Id, Id)> {
    // SAFETY: notification de fenêtre ; son objet est la NSWindow.
    unsafe {
        let window: Id = msg(notification, c"object", ());
        if window.is_null() {
            return None;
        }
        let view: Id = msg(window, c"contentView", ());
        (!view.is_null()).then_some((window, view))
    }
}

extern "C" fn window_should_close(_: Id, _: Sel, _: Id) -> Bool {
    // C'est l'application qui décide de fermer (sauvegarde, etc.).
    push(Event::CloseRequested);
    NO
}

extern "C" fn window_did_resize(_: Id, _: Sel, notification: Id) {
    if let Some((_, view)) = delegate_view(notification) {
        let (width, height) = view_size(view);
        push(Event::Resized { width, height });
    }
}

extern "C" fn window_did_change_backing(_: Id, _: Sel, notification: Id) {
    if let Some((window, view)) = delegate_view(notification) {
        update_scale(window, view);
        let (width, height) = view_size(view);
        push(Event::Resized { width, height });
        push(Event::Redraw);
    }
}

extern "C" fn window_did_resign_key(_: Id, _: Sel, _: Id) {
    push(Event::FocusLost);
}

extern "C" fn window_did_become_key(_: Id, _: Sel, _: Id) {
    push(Event::Redraw);
}

// ---------------------------------------------------------------- classes

#[cfg(target_arch = "aarch64")]
const BOOL_TYPES: &CStr = c"B@:";
#[cfg(not(target_arch = "aarch64"))]
const BOOL_TYPES: &CStr = c"c@:";
#[cfg(target_arch = "aarch64")]
const BOOL_OBJ_TYPES: &CStr = c"B@:@";
#[cfg(not(target_arch = "aarch64"))]
const BOOL_OBJ_TYPES: &CStr = c"c@:@";

/// Crée (une seule fois) une sous-classe de `superclass` avec ces méthodes.
fn define_class(name: &CStr, superclass: &CStr, methods: &[(&CStr, *const c_void, &CStr)]) -> Result<Class, WindowError> {
    let existing = class(name);
    if !existing.is_null() {
        return Ok(existing);
    }
    let superclass = class(superclass);
    if superclass.is_null() {
        return Err(WindowError::System("AppKit not found".into()));
    }
    // SAFETY: classe neuve, méthodes ajoutées avant l'enregistrement, signatures conformes aux types.
    unsafe {
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        if cls.is_null() {
            return Err(WindowError::System(format!("can't create the class {name:?}")));
        }
        for &(selector, imp, types) in methods {
            class_addMethod(cls, sel(selector), imp, types.as_ptr());
        }
        objc_registerClassPair(cls);
        Ok(cls)
    }
}

fn view_class() -> Result<Class, WindowError> {
    type Event3 = extern "C" fn(Id, Sel, Id);
    let ev = |f: Event3| f as *const c_void;
    let v = c"v@:@";
    define_class(
        c"PonzikView",
        c"NSView",
        &[
            (c"isFlipped", yes as extern "C" fn(Id, Sel) -> Bool as *const c_void, BOOL_TYPES),
            (c"acceptsFirstResponder", yes as extern "C" fn(Id, Sel) -> Bool as *const c_void, BOOL_TYPES),
            (c"wantsUpdateLayer", yes as extern "C" fn(Id, Sel) -> Bool as *const c_void, BOOL_TYPES),
            (c"acceptsFirstMouse:", yes_with as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void, BOOL_OBJ_TYPES),
            (c"updateLayer", update_layer as extern "C" fn(Id, Sel) as *const c_void, c"v@:"),
            (c"mouseDown:", ev(on_mouse_down), v),
            (c"mouseUp:", ev(on_mouse_up), v),
            (c"rightMouseDown:", ev(on_right_mouse_down), v),
            (c"rightMouseUp:", ev(on_right_mouse_up), v),
            (c"otherMouseDown:", ev(on_other_mouse_down), v),
            (c"otherMouseUp:", ev(on_other_mouse_up), v),
            (c"mouseMoved:", ev(on_mouse_moved), v),
            (c"mouseDragged:", ev(on_mouse_moved), v),
            (c"rightMouseDragged:", ev(on_mouse_moved), v),
            (c"otherMouseDragged:", ev(on_mouse_moved), v),
            (c"mouseEntered:", ev(on_mouse_entered), v),
            (c"mouseExited:", ev(on_mouse_exited), v),
            (c"cursorUpdate:", ev(on_cursor_update), v),
            (c"scrollWheel:", ev(on_scroll_wheel), v),
            (c"keyDown:", ev(on_key_down), v),
            (c"keyUp:", ev(on_key_up), v),
            (c"flagsChanged:", ev(on_flags_changed), v),
        ],
    )
}

fn delegate_class() -> Result<Class, WindowError> {
    type Event3 = extern "C" fn(Id, Sel, Id);
    let ev = |f: Event3| f as *const c_void;
    let v = c"v@:@";
    define_class(
        c"PonzikWindowDelegate",
        c"NSObject",
        &[
            (
                c"windowShouldClose:",
                window_should_close as extern "C" fn(Id, Sel, Id) -> Bool as *const c_void,
                BOOL_OBJ_TYPES,
            ),
            (c"windowDidResize:", ev(window_did_resize), v),
            (c"windowDidChangeBackingProperties:", ev(window_did_change_backing), v),
            (c"windowDidResignKey:", ev(window_did_resign_key), v),
            (c"windowDidBecomeKey:", ev(window_did_become_key), v),
        ],
    )
}

// ---------------------------------------------------------------- fenêtre

pub(crate) struct Window {
    app: Id,
    window: Id,
    view: Id,
    delegate: Id,
    color_space: CGColorSpaceRef,
    size: (u32, u32),
}

/// Menu d'application minimal : « Quitter » (Cmd+Q) demande la fermeture comme le bouton rouge.
unsafe fn install_menu(app: Id) {
    // SAFETY: séquence AppKit standard sur le thread principal, sous un bassin.
    unsafe {
        let alloc_init = |cls: &CStr| -> Id {
            let obj: Id = msg(class(cls), c"alloc", ());
            msg(obj, c"init", ())
        };
        let bar = alloc_init(c"NSMenu");
        let app_item = alloc_init(c"NSMenuItem");
        let () = msg(bar, c"addItem:", (app_item,));
        let app_menu = alloc_init(c"NSMenu");
        let quit: Id = msg(class(c"NSMenuItem"), c"alloc", ());
        let quit: Id = msg(
            quit,
            c"initWithTitle:action:keyEquivalent:",
            (ns_string("Quitter Ponzik"), sel(c"performClose:"), ns_string("q")),
        );
        let () = msg(app_menu, c"addItem:", (quit,));
        let () = msg(app_item, c"setSubmenu:", (app_menu,));
        let () = msg(app, c"setMainMenu:", (bar,));
        for obj in [quit, app_menu, app_item, bar] {
            let () = msg(obj, c"release", ());
        }
    }
}

impl Window {
    pub(crate) fn open(title: &str, width: u32, height: u32) -> Result<Self, WindowError> {
        let _pool = Pool::new();
        // SAFETY: appels AppKit sur le thread principal (vérifié d'abord) ; chaque objet
        // créé par alloc/init est soit gardé par `Window`, soit libéré ici.
        unsafe {
            let main: Bool = msg(class(c"NSThread"), c"isMainThread", ());
            if main == NO {
                return Err(WindowError::System("Cocoa requires the main thread for the window".into()));
            }
            let view_cls = view_class()?;
            let delegate_cls = delegate_class()?;
            RETINA.set(std::env::var("PONZIK_RETINA").is_ok_and(|v| v.trim() == "1"));

            let app: Id = msg(class(c"NSApplication"), c"sharedApplication", ());
            if app.is_null() {
                return Err(WindowError::NoDisplay("NSApplication indisponible".into()));
            }
            let _: Bool = msg(app, c"setActivationPolicy:", (NSApplicationActivationPolicyRegular,));
            let menu: Id = msg(app, c"mainMenu", ());
            if menu.is_null() {
                install_menu(app);
            }
            let () = msg(app, c"finishLaunching", ());

            // Fenêtre plus petite que l'écran si besoin (en points).
            let (mut w, mut h) = (f64::from(width), f64::from(height));
            let screen: Id = msg(class(c"NSScreen"), c"mainScreen", ());
            if !screen.is_null() {
                let visible = msg_rect(screen, c"visibleFrame");
                w = w.min(visible.size.width - 16.0).max(MIN_SIZE.0.min(visible.size.width));
                h = h.min(visible.size.height - 40.0).max(MIN_SIZE.1.min(visible.size.height));
            }
            let rect = NSRect { origin: NSPoint::default(), size: NSSize { width: w, height: h } };
            let style = NSWindowStyleMaskTitled
                | NSWindowStyleMaskClosable
                | NSWindowStyleMaskMiniaturizable
                | NSWindowStyleMaskResizable;
            let window: Id = msg(class(c"NSWindow"), c"alloc", ());
            let window: Id = msg(
                window,
                c"initWithContentRect:styleMask:backing:defer:",
                (rect, style, NSBackingStoreBuffered, NO),
            );
            if window.is_null() {
                return Err(WindowError::System("NSWindow creation refused".into()));
            }
            let () = msg(window, c"setReleasedWhenClosed:", (NO,));
            let () = msg(window, c"setTitle:", (ns_string(title),));
            let () = msg(window, c"setContentMinSize:", (NSSize { width: MIN_SIZE.0, height: MIN_SIZE.1 },));
            let dark: Id = msg(class(c"NSAppearance"), c"appearanceNamed:", (ns_string("NSAppearanceNameDarkAqua"),));
            if !dark.is_null() {
                let () = msg(window, c"setAppearance:", (dark,));
            }

            let delegate: Id = msg(delegate_cls, c"alloc", ());
            let delegate: Id = msg(delegate, c"init", ());
            let () = msg(window, c"setDelegate:", (delegate,));

            let view: Id = msg(view_cls, c"alloc", ());
            let view: Id = msg(view, c"initWithFrame:", (rect,));
            let () = msg(view, c"setWantsLayer:", (YES,));
            let layer: Id = msg(view, c"layer", ());
            if !layer.is_null() {
                // Doublement des pixels net plutôt que flou quand on rend en points.
                let () = msg(layer, c"setMagnificationFilter:", (ns_string("nearest"),));
                let () = msg(layer, c"setOpaque:", (YES,));
            }
            let area: Id = msg(class(c"NSTrackingArea"), c"alloc", ());
            let options = NSTrackingMouseEnteredAndExited
                | NSTrackingMouseMoved
                | NSTrackingCursorUpdate
                | NSTrackingActiveAlways
                | NSTrackingInVisibleRect;
            let area: Id =
                msg(area, c"initWithRect:options:owner:userInfo:", (NSRect::default(), options, view, null_mut::<c_void>()));
            let () = msg(view, c"addTrackingArea:", (area,));
            let () = msg(area, c"release", ());

            let () = msg(window, c"setContentView:", (view,));
            let _: Bool = msg(window, c"makeFirstResponder:", (view,));
            let () = msg(window, c"center", ());
            let () = msg(window, c"makeKeyAndOrderFront:", (null_mut::<c_void>(),));
            let () = msg(app, c"activateIgnoringOtherApps:", (YES,));

            update_scale(window, view);
            let color_space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
            let size = view_size(view);
            Ok(Window { app, window, view, delegate, color_space, size })
        }
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Transmet un événement à AppKit. Cocoa avale le relâchement d'une touche
    /// quand Cmd est tenu : on l'envoie alors directement à la fenêtre.
    fn dispatch(&self, event: Id) {
        // SAFETY: événement rendu par nextEventMatchingMask, objets vivants.
        unsafe {
            let kind: usize = msg(event, c"type", ());
            if kind == NSEventTypeKeyUp && event_flags(event) & NSEventModifierFlagCommand != 0 {
                let () = msg(self.window, c"sendEvent:", (event,));
            } else {
                let () = msg(self.app, c"sendEvent:", (event,));
            }
        }
    }

    /// Prochain événement d'AppKit, en attendant au plus jusqu'à `until` (NSDate).
    fn next_event(&self, until: Id) -> Id {
        // SAFETY: thread principal ; NSDefaultRunLoopMode est une constante de Foundation.
        unsafe {
            msg(self.app, c"nextEventMatchingMask:untilDate:inMode:dequeue:", (NSEventMaskAny, until, NSDefaultRunLoopMode, YES))
        }
    }

    fn drain(&mut self, out: &mut Vec<Event>) {
        QUEUE.with(|q| {
            for ev in q.borrow_mut().drain(..) {
                if let Event::Resized { width, height } = ev {
                    if (width, height) == self.size {
                        continue;
                    }
                    self.size = (width, height);
                }
                out.push(ev);
            }
        });
    }

    pub(crate) fn poll_events(&mut self, out: &mut Vec<Event>) {
        let _pool = Pool::new();
        // SAFETY: distantPast : n'attend pas.
        let past: Id = unsafe { msg(class(c"NSDate"), c"distantPast", ()) };
        loop {
            let event = self.next_event(past);
            if event.is_null() {
                break;
            }
            self.dispatch(event);
        }
        // SAFETY: thread principal.
        unsafe {
            let () = msg(self.app, c"updateWindows", ());
        }
        self.drain(out);
    }

    pub(crate) fn wait_events(&mut self, timeout: Duration, out: &mut Vec<Event>) {
        let queued = QUEUE.with(|q| !q.borrow().is_empty());
        if !queued {
            let _pool = Pool::new();
            // SAFETY: date autolibérée, vivante jusqu'à la fin du bassin.
            let until: Id = unsafe { msg(class(c"NSDate"), c"dateWithTimeIntervalSinceNow:", (timeout.as_secs_f64(),)) };
            let event = self.next_event(until);
            if !event.is_null() {
                self.dispatch(event);
            }
        }
        self.poll_events(out);
    }

    pub(crate) fn present(&mut self, pixels: &[u32], width: u32, height: u32) -> Result<(), WindowError> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let _pool = Pool::new();
        let (w, h) = (width as usize, height as usize);
        let bytes = w * h * 4;
        // SAFETY: `pixels` contient au moins w * h pixels (vérifié par l'appelant) ;
        // CFDataCreate en fait une copie, que l'image garde aussi longtemps que le calque.
        unsafe {
            let data = CFDataCreate(null(), pixels.as_ptr().cast(), bytes as isize);
            if data.is_null() {
                return Err(WindowError::System("CFDataCreate failed".into()));
            }
            let provider = CGDataProviderCreateWithCFData(data);
            CFRelease(data);
            if provider.is_null() {
                return Err(WindowError::System("CGDataProviderCreateWithCFData failed".into()));
            }
            let image = CGImageCreate(w, h, 8, 32, w * 4, self.color_space, BITMAP_INFO, provider, null(), false, 0);
            CGDataProviderRelease(provider);
            if image.is_null() {
                return Err(WindowError::System("CGImageCreate failed".into()));
            }
            let layer: Id = msg(self.view, c"layer", ());
            if !layer.is_null() {
                let () = msg(layer, c"setContents:", (image,));
            }
            CGImageRelease(image);
            // Sans attendre le prochain tour de boucle : l'image part tout de suite au compositeur.
            let transaction = class(c"CATransaction");
            if !transaction.is_null() {
                let () = msg(transaction, c"flush", ());
            }
        }
        Ok(())
    }

    pub(crate) fn set_cursor(&mut self, cursor: Cursor) {
        if CURSOR.get() == cursor {
            return;
        }
        CURSOR.set(cursor);
        // Appliqué tout de suite si la souris est sur la vue ; sinon à son retour (cursorUpdate:).
        if INSIDE.get() {
            apply_cursor();
        }
    }

    pub(crate) fn backend_name(&self) -> &'static str {
        if RETINA.get() { "Cocoa · CoreGraphics (Retina natif)" } else { "Cocoa · CoreGraphics" }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _pool = Pool::new();
        // SAFETY: objets créés par `open`, libérés une seule fois, sur le thread principal.
        unsafe {
            let () = msg(self.window, c"setDelegate:", (null_mut::<c_void>(),));
            let () = msg(self.window, c"orderOut:", (null_mut::<c_void>(),));
            let () = msg(self.window, c"close", ());
            for obj in [self.view, self.delegate, self.window] {
                let () = msg(obj, c"release", ());
            }
            CGColorSpaceRelease(self.color_space);
        }
    }
}
