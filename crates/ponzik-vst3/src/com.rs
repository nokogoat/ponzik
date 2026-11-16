//! Objets COM côté hôte et pointeurs intelligents sur les interfaces du plugin.
//!
//! Les objets de l'hôte sont des [`Obj`] : table virtuelle, compteur de
//! références atomique, données. Ceux alloués sur le tas sont libérés au dernier
//! `release` ; les files de valeurs de paramètres vivent dans leur liste parente
//! et ne sont libérées qu'avec elle.

use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::abi::*;

// --- Pointeurs sur les interfaces du plugin --------------------------------

/// Référence possédée sur une interface du plugin ; `release` à la destruction.
pub(crate) struct Com<I: Interface> {
    ptr: NonNull<I>,
    _i: PhantomData<I>,
}

impl<I: Interface> Com<I> {
    /// Prend possession d'une référence déjà comptée (sortie d'une fabrique ou de `queryInterface`).
    ///
    /// # Safety
    /// `ptr` est nul ou désigne un objet COM vivant implémentant `I`.
    pub unsafe fn from_raw(ptr: *mut c_void) -> Option<Self> {
        NonNull::new(ptr.cast()).map(|ptr| Com { ptr, _i: PhantomData })
    }

    pub fn as_ptr<T>(&self) -> *mut T {
        self.ptr.as_ptr().cast()
    }

    /// Table virtuelle, de type `V` (celle de `I`).
    ///
    /// # Safety
    /// `V` est la table virtuelle de `I`.
    pub unsafe fn vtbl<V>(&self) -> &V {
        // SAFETY : premier champ de l'objet, table statique du plugin.
        unsafe { &**(self.ptr.as_ptr() as *const *const V) }
    }

    fn unknown(&self) -> &FUnknownVtbl {
        // SAFETY : toutes les tables commencent par celle de FUnknown.
        unsafe { self.vtbl::<FUnknownVtbl>() }
    }

    /// `queryInterface` vers `J`.
    pub fn query<J: Interface>(&self) -> Option<Com<J>> {
        let mut out = null_mut();
        // SAFETY : objet vivant, sortie locale.
        let r = unsafe { (self.unknown().query_interface)(self.as_ptr(), &J::IID, &mut out) };
        if r == K_RESULT_OK { unsafe { Com::from_raw(out) } } else { None }
    }
}

impl<I: Interface> Drop for Com<I> {
    fn drop(&mut self) {
        // SAFETY : on rend la référence possédée.
        unsafe { (self.unknown().release)(self.as_ptr()) };
    }
}

// --- Objets de l'hôte ------------------------------------------------------

/// Données d'un objet COM de l'hôte.
pub(crate) trait HostObject: Sized + 'static {
    type Vtbl: 'static;
    const VTBL: &'static Self::Vtbl;
    /// Interfaces exposées en plus de FUnknown.
    const IIDS: &'static [Tuid];
    /// Libéré au dernier `release` (faux : possédé par un parent).
    const HEAP: bool = true;
}

#[repr(C)]
pub(crate) struct Obj<T: HostObject> {
    vtbl: &'static T::Vtbl,
    refs: AtomicU32,
    pub data: T,
}

/// En-tête commun à tous les [`Obj`] (le compteur suit le pointeur de table).
#[repr(C)]
struct Header {
    vtbl: *const c_void,
    refs: AtomicU32,
}

impl<T: HostObject> Obj<T> {
    /// Objet intégré à un parent, jamais libéré par `release`.
    pub fn embedded(data: T) -> Self {
        Obj { vtbl: T::VTBL, refs: AtomicU32::new(1), data }
    }

    /// Données depuis le pointeur `this` reçu du plugin.
    ///
    /// # Safety
    /// `this` désigne un `Obj<T>` vivant, sans autre emprunt actif.
    pub unsafe fn data<'a>(this: *mut c_void) -> &'a mut T {
        unsafe { &mut (*(this as *mut Obj<T>)).data }
    }
}

unsafe extern "system" fn query_interface<T: HostObject>(
    this: *mut c_void,
    iid: *const Tuid,
    obj: *mut *mut c_void,
) -> TResult {
    if obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : identifiant de 16 octets fourni par le plugin.
    let known = !iid.is_null() && {
        let iid = unsafe { &*iid };
        *iid == FUNKNOWN_IID || T::IIDS.contains(iid)
    };
    // SAFETY : sortie fournie par le plugin.
    unsafe {
        if known {
            add_ref(this);
            *obj = this;
            K_RESULT_OK
        } else {
            *obj = null_mut();
            K_NO_INTERFACE
        }
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    // SAFETY : tout `Obj` commence par `Header`.
    unsafe { (*(this as *const Header)).refs.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn release<T: HostObject>(this: *mut c_void) -> u32 {
    // SAFETY : tout `Obj` commence par `Header`.
    let refs = unsafe { &(*(this as *const Header)).refs };
    let prev = refs.fetch_sub(1, Ordering::AcqRel);
    if prev == 0 {
        // Libération en trop du plugin : on ignore plutôt que de corrompre la mémoire.
        refs.store(0, Ordering::Relaxed);
        return 0;
    }
    if prev == 1 && T::HEAP {
        // SAFETY : dernière référence d'un objet créé par `Owned::new`.
        drop(unsafe { Box::from_raw(this as *mut Obj<T>) });
    }
    prev - 1
}

pub(crate) const fn unknown_vtbl<T: HostObject>() -> FUnknownVtbl {
    FUnknownVtbl { query_interface: query_interface::<T>, add_ref, release: release::<T> }
}

/// Référence de l'hôte sur un objet [`Obj`] du tas.
pub(crate) struct Owned<T: HostObject>(NonNull<Obj<T>>);

impl<T: HostObject> Owned<T> {
    pub fn new(data: T) -> Self {
        Owned(NonNull::from(Box::leak(Box::new(Obj::embedded(data)))))
    }

    pub fn as_ptr<I>(&self) -> *mut I {
        self.0.as_ptr().cast()
    }

    /// Accès aux données hors des appels au plugin.
    pub fn get(&mut self) -> &mut T {
        // SAFETY : le plugin n'y accède que pendant nos appels, sur ce thread.
        unsafe { &mut (*self.0.as_ptr()).data }
    }

    /// Donne la référence au plugin (sortie d'un `createInstance`).
    pub fn into_raw(self) -> *mut c_void {
        let p = self.0.as_ptr().cast();
        std::mem::forget(self);
        p
    }
}

impl<T: HostObject> Drop for Owned<T> {
    fn drop(&mut self) {
        // SAFETY : on rend la référence de l'hôte.
        unsafe { release::<T>(self.0.as_ptr().cast()) };
    }
}

// SAFETY : les objets de l'hôte ne contiennent que des données possédées et un
// compteur atomique ; l'accès mutable est sérialisé par les règles d'appel VST 3.
unsafe impl<T: HostObject> Send for Owned<T> {}

// --- IHostApplication ------------------------------------------------------

pub(crate) struct HostApplication;

impl HostObject for HostApplication {
    type Vtbl = IHostApplicationVtbl;
    const VTBL: &'static IHostApplicationVtbl = &IHostApplicationVtbl {
        unknown: unknown_vtbl::<Self>(),
        get_name: host_get_name,
        create_instance: host_create_instance,
    };
    const IIDS: &'static [Tuid] = &[IHOST_APPLICATION_IID];
}

unsafe extern "system" fn host_get_name(_: *mut c_void, name: *mut u16) -> TResult {
    if name.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : `String128` fourni par le plugin.
    copy_wstr(unsafe { std::slice::from_raw_parts_mut(name, 128) }, "Ponzik");
    K_RESULT_OK
}

unsafe extern "system" fn host_create_instance(
    _: *mut c_void,
    cid: *const Tuid,
    iid: *const Tuid,
    obj: *mut *mut c_void,
) -> TResult {
    if obj.is_null() || cid.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : identifiants de 16 octets fournis par le plugin.
    let (cid, iid) = unsafe { (*cid, if iid.is_null() { *cid } else { *iid }) };
    let created = if cid == IMESSAGE_IID && (iid == IMESSAGE_IID || iid == FUNKNOWN_IID) {
        Some(Owned::new(Message::default()).into_raw())
    } else if cid == IATTRIBUTE_LIST_IID && (iid == IATTRIBUTE_LIST_IID || iid == FUNKNOWN_IID) {
        Some(Owned::new(AttributeList::default()).into_raw())
    } else {
        None
    };
    // SAFETY : sortie fournie par le plugin.
    unsafe { *obj = created.unwrap_or(null_mut()) };
    if created.is_some() { K_RESULT_OK } else { K_NO_INTERFACE }
}

// --- IComponentHandler -----------------------------------------------------

/// Récepteur des éditions du contrôleur ; sans interface graphique, elles sont ignorées.
pub(crate) struct ComponentHandler;

impl HostObject for ComponentHandler {
    type Vtbl = IComponentHandlerVtbl;
    const VTBL: &'static IComponentHandlerVtbl = &IComponentHandlerVtbl {
        unknown: unknown_vtbl::<Self>(),
        begin_edit: handler_edit,
        perform_edit: handler_perform,
        end_edit: handler_edit,
        restart_component: handler_restart,
    };
    const IIDS: &'static [Tuid] = &[ICOMPONENT_HANDLER_IID];
}

unsafe extern "system" fn handler_edit(_: *mut c_void, _: ParamId) -> TResult {
    K_RESULT_OK
}

unsafe extern "system" fn handler_perform(_: *mut c_void, _: ParamId, _: ParamValue) -> TResult {
    K_RESULT_OK
}

unsafe extern "system" fn handler_restart(_: *mut c_void, _: i32) -> TResult {
    K_NOT_IMPLEMENTED
}

// --- IMessage / IAttributeList ---------------------------------------------

#[derive(Default)]
pub(crate) struct Message {
    id: Option<CString>,
    attributes: Option<Owned<AttributeList>>,
}

impl HostObject for Message {
    type Vtbl = IMessageVtbl;
    const VTBL: &'static IMessageVtbl = &IMessageVtbl {
        unknown: unknown_vtbl::<Self>(),
        get_message_id: msg_get_id,
        set_message_id: msg_set_id,
        get_attributes: msg_attributes,
    };
    const IIDS: &'static [Tuid] = &[IMESSAGE_IID];
}

unsafe extern "system" fn msg_get_id(this: *mut c_void) -> *const c_char {
    // SAFETY : `this` est un `Obj<Message>`.
    unsafe { Obj::<Message>::data(this) }.id.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())
}

unsafe extern "system" fn msg_set_id(this: *mut c_void, id: *const c_char) {
    // SAFETY : `this` est un `Obj<Message>`, `id` une chaîne C ou nul.
    let m = unsafe { Obj::<Message>::data(this) };
    m.id = (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_owned());
}

unsafe extern "system" fn msg_attributes(this: *mut c_void) -> *mut IAttributeList {
    // SAFETY : `this` est un `Obj<Message>`. La liste reste possédée par le message.
    let m = unsafe { Obj::<Message>::data(this) };
    m.attributes.get_or_insert_with(|| Owned::new(AttributeList::default())).as_ptr()
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

#[derive(Default)]
pub(crate) struct AttributeList {
    items: Vec<(CString, Attr)>,
}

impl AttributeList {
    fn set(&mut self, id: *const c_char, value: Attr) -> TResult {
        if id.is_null() {
            return K_INVALID_ARGUMENT;
        }
        // SAFETY : chaîne C fournie par le plugin.
        let id = unsafe { CStr::from_ptr(id) };
        match self.items.iter_mut().find(|(k, _)| k.as_c_str() == id) {
            Some(item) => item.1 = value,
            None => self.items.push((id.to_owned(), value)),
        }
        K_RESULT_OK
    }

    fn get(&self, id: *const c_char) -> Option<&Attr> {
        if id.is_null() {
            return None;
        }
        // SAFETY : chaîne C fournie par le plugin.
        let id = unsafe { CStr::from_ptr(id) };
        self.items.iter().find(|(k, _)| k.as_c_str() == id).map(|(_, v)| v)
    }
}

impl HostObject for AttributeList {
    type Vtbl = IAttributeListVtbl;
    const VTBL: &'static IAttributeListVtbl = &IAttributeListVtbl {
        unknown: unknown_vtbl::<Self>(),
        set_int: attr_set_int,
        get_int: attr_get_int,
        set_float: attr_set_float,
        get_float: attr_get_float,
        set_string: attr_set_string,
        get_string: attr_get_string,
        set_binary: attr_set_binary,
        get_binary: attr_get_binary,
    };
    const IIDS: &'static [Tuid] = &[IATTRIBUTE_LIST_IID];
}

fn attrs<'a>(this: *mut c_void) -> &'a mut AttributeList {
    // SAFETY : `this` est un `Obj<AttributeList>` (appelé uniquement par sa table).
    unsafe { Obj::<AttributeList>::data(this) }
}

unsafe extern "system" fn attr_set_int(this: *mut c_void, id: *const c_char, v: i64) -> TResult {
    attrs(this).set(id, Attr::Int(v))
}

unsafe extern "system" fn attr_get_int(this: *mut c_void, id: *const c_char, out: *mut i64) -> TResult {
    match attrs(this).get(id) {
        // SAFETY : sortie fournie par le plugin.
        Some(&Attr::Int(v)) if !out.is_null() => unsafe { *out = v },
        _ => return K_RESULT_FALSE,
    }
    K_RESULT_OK
}

unsafe extern "system" fn attr_set_float(this: *mut c_void, id: *const c_char, v: f64) -> TResult {
    attrs(this).set(id, Attr::Float(v))
}

unsafe extern "system" fn attr_get_float(this: *mut c_void, id: *const c_char, out: *mut f64) -> TResult {
    match attrs(this).get(id) {
        // SAFETY : sortie fournie par le plugin.
        Some(&Attr::Float(v)) if !out.is_null() => unsafe { *out = v },
        _ => return K_RESULT_FALSE,
    }
    K_RESULT_OK
}

unsafe extern "system" fn attr_set_string(this: *mut c_void, id: *const c_char, s: *const u16) -> TResult {
    if s.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let mut v = Vec::new();
    // SAFETY : chaîne UTF-16 terminée par zéro fournie par le plugin.
    while unsafe { *s.add(v.len()) } != 0 {
        v.push(unsafe { *s.add(v.len()) });
    }
    attrs(this).set(id, Attr::Str(v))
}

unsafe extern "system" fn attr_get_string(this: *mut c_void, id: *const c_char, out: *mut u16, bytes: u32) -> TResult {
    let Some(Attr::Str(s)) = attrs(this).get(id) else { return K_RESULT_FALSE };
    let cap = bytes as usize / 2;
    if out.is_null() || cap == 0 {
        return K_INVALID_ARGUMENT;
    }
    let n = s.len().min(cap - 1);
    // SAFETY : tampon de `bytes` octets fourni par le plugin.
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    K_RESULT_OK
}

unsafe extern "system" fn attr_set_binary(this: *mut c_void, id: *const c_char, d: *const c_void, n: u32) -> TResult {
    let bytes = if d.is_null() || n == 0 {
        Vec::new()
    } else {
        // SAFETY : tampon de `n` octets fourni par le plugin.
        unsafe { std::slice::from_raw_parts(d as *const u8, n as usize) }.to_vec()
    };
    attrs(this).set(id, Attr::Bin(bytes))
}

unsafe extern "system" fn attr_get_binary(
    this: *mut c_void,
    id: *const c_char,
    d: *mut *const c_void,
    n: *mut u32,
) -> TResult {
    let Some(Attr::Bin(b)) = attrs(this).get(id) else { return K_RESULT_FALSE };
    if d.is_null() || n.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : sorties fournies par le plugin ; données valides tant que la liste vit.
    unsafe {
        *d = b.as_ptr().cast();
        *n = b.len() as u32;
    }
    K_RESULT_OK
}

// --- IBStream en mémoire ----------------------------------------------------

#[derive(Default)]
pub(crate) struct MemoryStream {
    pub buf: Vec<u8>,
    pub pos: usize,
}

impl HostObject for MemoryStream {
    type Vtbl = IBStreamVtbl;
    const VTBL: &'static IBStreamVtbl = &IBStreamVtbl {
        unknown: unknown_vtbl::<Self>(),
        read: stream_read,
        write: stream_write,
        seek: stream_seek,
        tell: stream_tell,
    };
    const IIDS: &'static [Tuid] = &[IBSTREAM_IID];
}

fn stream<'a>(this: *mut c_void) -> &'a mut MemoryStream {
    // SAFETY : `this` est un `Obj<MemoryStream>` (appelé uniquement par sa table).
    unsafe { Obj::<MemoryStream>::data(this) }
}

unsafe extern "system" fn stream_read(this: *mut c_void, buf: *mut c_void, n: i32, read: *mut i32) -> TResult {
    let s = stream(this);
    if n < 0 || (buf.is_null() && n > 0) {
        return K_INVALID_ARGUMENT;
    }
    let k = (n as usize).min(s.buf.len().saturating_sub(s.pos));
    // SAFETY : tampon de `n` octets fourni par le plugin ; `pos + k` reste dans `buf`.
    unsafe {
        if k > 0 {
            std::ptr::copy_nonoverlapping(s.buf.as_ptr().add(s.pos), buf as *mut u8, k);
        }
        if !read.is_null() {
            *read = k as i32;
        }
    }
    s.pos += k;
    K_RESULT_OK
}

unsafe extern "system" fn stream_write(this: *mut c_void, buf: *mut c_void, n: i32, written: *mut i32) -> TResult {
    let s = stream(this);
    if n < 0 || (buf.is_null() && n > 0) {
        return K_INVALID_ARGUMENT;
    }
    let n = n as usize;
    if s.buf.len() < s.pos + n {
        s.buf.resize(s.pos + n, 0);
    }
    // SAFETY : tampon de `n` octets fourni par le plugin ; `buf` agrandi à `pos + n`.
    unsafe {
        if n > 0 {
            std::ptr::copy_nonoverlapping(buf as *const u8, s.buf.as_mut_ptr().add(s.pos), n);
        }
        if !written.is_null() {
            *written = n as i32;
        }
    }
    s.pos += n;
    K_RESULT_OK
}

unsafe extern "system" fn stream_seek(this: *mut c_void, pos: i64, mode: i32, result: *mut i64) -> TResult {
    let s = stream(this);
    let base = match mode {
        K_IB_SEEK_SET => 0,
        K_IB_SEEK_CUR => s.pos as i64,
        K_IB_SEEK_END => s.buf.len() as i64,
        _ => return K_INVALID_ARGUMENT,
    };
    let Some(p) = base.checked_add(pos).filter(|&p| p >= 0) else { return K_INVALID_ARGUMENT };
    s.pos = p as usize;
    if !result.is_null() {
        // SAFETY : sortie fournie par le plugin.
        unsafe { *result = p };
    }
    K_RESULT_OK
}

unsafe extern "system" fn stream_tell(this: *mut c_void, pos: *mut i64) -> TResult {
    if pos.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : sortie fournie par le plugin.
    unsafe { *pos = stream(this).pos as i64 };
    K_RESULT_OK
}

// --- IParameterChanges / IParamValueQueue ------------------------------------

/// Points par file et par bloc ; au-delà, le dernier point est remplacé.
pub(crate) const QUEUE_POINTS: usize = 32;

pub(crate) struct ParamQueue {
    id: ParamId,
    points: [(i32, ParamValue); QUEUE_POINTS],
    len: usize,
}

impl ParamQueue {
    /// Insère un point, trié par position ; une position déjà présente est remplacée.
    fn add(&mut self, offset: i32, value: ParamValue) -> usize {
        let pts = &mut self.points[..self.len];
        let i = pts.iter().position(|p| p.0 >= offset).unwrap_or(pts.len());
        if i < pts.len() && pts[i].0 == offset {
            pts[i].1 = value;
            return i;
        }
        if self.len == QUEUE_POINTS {
            let last = QUEUE_POINTS - 1;
            self.points[last] = (offset.max(self.points[last].0), value);
            return last;
        }
        self.points.copy_within(i..self.len, i + 1);
        self.points[i] = (offset, value);
        self.len += 1;
        i
    }

    pub fn id(&self) -> ParamId {
        self.id
    }

    pub fn last(&self) -> Option<ParamValue> {
        self.len.checked_sub(1).map(|i| self.points[i].1)
    }
}

impl HostObject for ParamQueue {
    type Vtbl = IParamValueQueueVtbl;
    const VTBL: &'static IParamValueQueueVtbl = &IParamValueQueueVtbl {
        unknown: unknown_vtbl::<Self>(),
        get_parameter_id: queue_id,
        get_point_count: queue_count,
        get_point: queue_get,
        add_point: queue_add,
    };
    const IIDS: &'static [Tuid] = &[IPARAM_VALUE_QUEUE_IID];
    const HEAP: bool = false;
}

fn queue<'a>(this: *mut c_void) -> &'a mut ParamQueue {
    // SAFETY : `this` est un `Obj<ParamQueue>` (appelé uniquement par sa table).
    unsafe { Obj::<ParamQueue>::data(this) }
}

unsafe extern "system" fn queue_id(this: *mut c_void) -> ParamId {
    queue(this).id
}

unsafe extern "system" fn queue_count(this: *mut c_void) -> i32 {
    queue(this).len as i32
}

unsafe extern "system" fn queue_get(this: *mut c_void, i: i32, offset: *mut i32, value: *mut ParamValue) -> TResult {
    let q = queue(this);
    if i < 0 || i as usize >= q.len || offset.is_null() || value.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : sorties fournies par le plugin.
    unsafe { (*offset, *value) = q.points[i as usize] };
    K_RESULT_OK
}

unsafe extern "system" fn queue_add(this: *mut c_void, offset: i32, value: ParamValue, index: *mut i32) -> TResult {
    let i = queue(this).add(offset, value);
    if !index.is_null() {
        // SAFETY : sortie fournie par le plugin.
        unsafe { *index = i as i32 };
    }
    K_RESULT_OK
}

/// Liste de changements à capacité fixe : aucune allocation pendant le traitement.
pub(crate) struct ParamChanges {
    queues: Box<[Obj<ParamQueue>]>,
    used: usize,
}

impl ParamChanges {
    pub fn new(capacity: usize) -> Owned<Self> {
        let queues = (0..capacity.max(1))
            .map(|_| Obj::embedded(ParamQueue { id: K_NO_PARAM_ID, points: [(0, 0.0); QUEUE_POINTS], len: 0 }))
            .collect();
        Owned::new(ParamChanges { queues, used: 0 })
    }

    /// File du paramètre `id`, créée si besoin (`None` si la liste est pleine).
    fn queue_for(&mut self, id: ParamId) -> Option<usize> {
        if let Some(i) = self.queues[..self.used].iter().position(|q| q.data.id == id) {
            return Some(i);
        }
        let q = &mut self.queues.get_mut(self.used)?.data;
        q.id = id;
        q.len = 0;
        self.used += 1;
        Some(self.used - 1)
    }

    pub fn add(&mut self, id: ParamId, offset: i32, value: ParamValue) {
        if let Some(i) = self.queue_for(id) {
            self.queues[i].data.add(offset, value);
        }
    }

    pub fn queues(&self) -> impl Iterator<Item = &ParamQueue> {
        self.queues[..self.used].iter().map(|q| &q.data)
    }

    pub fn clear(&mut self) {
        self.used = 0;
    }
}

impl HostObject for ParamChanges {
    type Vtbl = IParameterChangesVtbl;
    const VTBL: &'static IParameterChangesVtbl = &IParameterChangesVtbl {
        unknown: unknown_vtbl::<Self>(),
        get_parameter_count: changes_count,
        get_parameter_data: changes_get,
        add_parameter_data: changes_add,
    };
    const IIDS: &'static [Tuid] = &[IPARAMETER_CHANGES_IID];
}

fn changes<'a>(this: *mut c_void) -> &'a mut ParamChanges {
    // SAFETY : `this` est un `Obj<ParamChanges>` (appelé uniquement par sa table).
    unsafe { Obj::<ParamChanges>::data(this) }
}

unsafe extern "system" fn changes_count(this: *mut c_void) -> i32 {
    changes(this).used as i32
}

unsafe extern "system" fn changes_get(this: *mut c_void, i: i32) -> *mut IParamValueQueue {
    let c = changes(this);
    if i < 0 || i as usize >= c.used {
        return null_mut();
    }
    &mut c.queues[i as usize] as *mut Obj<ParamQueue> as *mut IParamValueQueue
}

unsafe extern "system" fn changes_add(this: *mut c_void, id: *const ParamId, index: *mut i32) -> *mut IParamValueQueue {
    if id.is_null() {
        return null_mut();
    }
    let c = changes(this);
    // SAFETY : identifiant fourni par le plugin.
    let Some(i) = c.queue_for(unsafe { *id }) else { return null_mut() };
    if !index.is_null() {
        // SAFETY : sortie fournie par le plugin.
        unsafe { *index = i as i32 };
    }
    &mut c.queues[i] as *mut Obj<ParamQueue> as *mut IParamValueQueue
}

// --- IEventList ------------------------------------------------------------

/// Liste d'événements à capacité fixe : au-delà, les événements du bloc sont perdus.
pub(crate) struct EventList {
    events: Box<[Event]>,
    len: usize,
}

impl EventList {
    pub fn new(capacity: usize) -> Owned<Self> {
        let empty = Event {
            bus_index: 0,
            sample_offset: 0,
            ppq_position: 0.0,
            flags: 0,
            kind: 0,
            data: EventData { raw: [0; 3] },
        };
        Owned::new(EventList { events: vec![empty; capacity].into_boxed_slice(), len: 0 })
    }

    /// Ajoute un événement en gardant l'ordre des positions (stable à position égale).
    pub fn push(&mut self, e: Event) -> bool {
        if self.len == self.events.len() {
            return false;
        }
        let evs = &mut self.events[..self.len];
        let i = evs.iter().rposition(|x| x.sample_offset <= e.sample_offset).map_or(0, |i| i + 1);
        self.events.copy_within(i..self.len, i + 1);
        self.events[i] = e;
        self.len += 1;
        true
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl HostObject for EventList {
    type Vtbl = IEventListVtbl;
    const VTBL: &'static IEventListVtbl = &IEventListVtbl {
        unknown: unknown_vtbl::<Self>(),
        get_event_count: events_count,
        get_event: events_get,
        add_event: events_add,
    };
    const IIDS: &'static [Tuid] = &[IEVENT_LIST_IID];
}

fn events<'a>(this: *mut c_void) -> &'a mut EventList {
    // SAFETY : `this` est un `Obj<EventList>` (appelé uniquement par sa table).
    unsafe { Obj::<EventList>::data(this) }
}

unsafe extern "system" fn events_count(this: *mut c_void) -> i32 {
    events(this).len as i32
}

unsafe extern "system" fn events_get(this: *mut c_void, i: i32, out: *mut Event) -> TResult {
    let l = events(this);
    if i < 0 || i as usize >= l.len || out.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : sortie fournie par le plugin.
    unsafe { *out = l.events[i as usize] };
    K_RESULT_OK
}

unsafe extern "system" fn events_add(this: *mut c_void, e: *mut Event) -> TResult {
    if e.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // SAFETY : événement fourni par le plugin.
    if events(this).push(unsafe { *e }) { K_RESULT_OK } else { K_OUT_OF_MEMORY }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(offset: i32, pitch: i16) -> Event {
        let note_on = NoteOnEvent { channel: 0, pitch, tuning: 0.0, velocity: 1.0, length: 0, note_id: -1 };
        Event { bus_index: 0, sample_offset: offset, ppq_position: 0.0, flags: 0, kind: 0, data: EventData { note_on } }
    }

    #[test]
    fn event_list_keeps_offsets_sorted() {
        let mut l = EventList::new(3);
        assert!(l.get().push(note(10, 1)));
        assert!(l.get().push(note(0, 2)));
        assert!(l.get().push(note(10, 3)));
        assert!(!l.get().push(note(5, 4)));
        let p: *mut c_void = l.as_ptr();
        let mut e = note(0, 0);
        let order: Vec<_> = (0..3)
            .map(|i| {
                assert_eq!(unsafe { events_get(p, i, &mut e) }, K_RESULT_OK);
                unsafe { e.data.note_on.pitch }
            })
            .collect();
        assert_eq!(order, [2, 1, 3]);
    }

    #[test]
    fn parameter_changes_are_bounded() {
        let mut c = ParamChanges::new(2);
        c.get().add(1, 0, 0.1);
        c.get().add(1, 0, 0.2);
        c.get().add(2, 5, 0.3);
        c.get().add(3, 0, 0.4);
        let lasts: Vec<_> = c.get().queues().map(|q| (q.id, q.len, q.last())).collect();
        assert_eq!(lasts, [(1, 1, Some(0.2)), (2, 1, Some(0.3))]);
        for i in 0..40 {
            c.get().add(2, i, i as f64);
        }
        assert_eq!(c.get().queues().nth(1).map(|q| (q.len, q.last())), Some((QUEUE_POINTS, Some(39.0))));
        c.get().clear();
        assert_eq!(c.get().queues().count(), 0);
    }

    #[test]
    fn refcount_and_query() {
        let s = Owned::new(MemoryStream::default());
        let p: *mut c_void = s.as_ptr();
        let mut out = null_mut();
        unsafe {
            assert_eq!(query_interface::<MemoryStream>(p, &IBSTREAM_IID, &mut out), K_RESULT_OK);
            assert_eq!(out, p);
            assert_eq!(query_interface::<MemoryStream>(p, &IEVENT_LIST_IID, &mut out), K_NO_INTERFACE);
            assert!(out.is_null());
            assert_eq!(release::<MemoryStream>(p), 1);
        }
        drop(s);
    }

    #[test]
    fn memory_stream_roundtrip() {
        let s = Owned::new(MemoryStream::default());
        let p: *mut c_void = s.as_ptr();
        let mut data = [1u8, 2, 3, 4];
        let mut n = 0;
        let mut pos = 0;
        unsafe {
            stream_write(p, data.as_mut_ptr().cast(), 4, &mut n);
            assert_eq!(n, 4);
            stream_seek(p, 1, K_IB_SEEK_SET, &mut pos);
            let mut out = [0u8; 8];
            stream_read(p, out.as_mut_ptr().cast(), 8, &mut n);
            assert_eq!((n, &out[..3]), (3, &[2u8, 3, 4][..]));
        }
    }
}
