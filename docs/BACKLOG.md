# Backlog produit

Novembre 2026. Comparé à FL Studio 2025.2, Ableton Live 12.3, Bitwig Studio 5.3,
Logic Pro 11, Reaper 7, Studio One 7, Cubase 14, et aux plugins les plus utilisés
(Serum 2, Vital, Pro-Q 4, Pro-C, Pro-L, Valhalla, Soundtoys, OTT, Kickstart,
Gross Beat, Melodyne, Ozone).

**Mode d'emploi.** Prendre les éléments de la Vague 1 dans l'ordre. Chaque ligne
indique la taille (**S** ≤ 1 h, **M** ½ à 2 jours, **L** une semaine ou plus),
les zones touchées (**engine** = `ponzik-engine`, **dsp** = `ponzik-dsp`,
**project** = `ponzik-project`, **ui** = `apps/ponzik/src/gui`, **window** =
`ponzik-window`) et ce dont l'élément dépend.

**Déjà fait mais encore marqué ⬜ dans `FEATURES.md`** (à corriger) : hôte CLAP
(audio, paramètres, notes), enregistrement audio sur les pistes du mixer avec
écoute, import et export `.mid`, presets d'appareils, projets récents, swing du
Channel Rack, couloir de vélocité du piano roll, inversion de phase (Utilitaire).

---

## 1. Règles d'interface : le calme d'Ableton, la puissance de FL

FL Studio est dense : fenêtres flottantes, chaque plugin a son propre style,
beaucoup de petits boutons sans libellé. Live et Bitwig paraissent calmes parce
qu'ils ont une seule fenêtre, une seule grammaire visuelle et un panneau du bas
qui affiche toujours le détail de la sélection. Les règles à suivre, à vérifier
à chaque revue d'interface :

1. **Une sélection, un panneau de détail.** Ce qu'on sélectionne (canal, clip,
   piste, note) s'affiche dans le panneau du bas (appareils ou clip). Pas de
   fenêtres flottantes par défaut.
2. **Barre d'aide permanente.** Au survol, elle affiche le nom, une phrase qui
   explique et le raccourci. Tout widget doit avoir un texte d'aide : c'est
   l'Info View d'Ableton et la hint bar de FL.
3. **Divulgation progressive.** Chaque appareil montre d'abord 4 à 8 macros avec
   son visuel, et le reste dans « Avancé ». Les groupes de paramètres
   (`ParamInfo::group`) servent d'onglets. Rien de caché n'est inaccessible : la
   palette Ctrl+K trouve tout.
4. **Une couleur veut dire une chose.** La couleur d'accent sert à la sélection et
   à la lecture, le rouge à l'enregistrement et à l'écrêtage, la couleur du canal
   à son identité. Pas de dégradés décoratifs.
5. **Pictogrammes et libellés ensemble.** Un pictogramme seul ne suffit que pour
   les gestes universels (lecture, enregistrement, boucle, métronome). Partout
   ailleurs, on met un mot.
6. **Les gestes de FL, les mêmes partout.** Le clic gauche pose, le clic droit
   maintenu efface, la molette règle, Ctrl+glisser copie, Maj ou Ctrl donnent la
   précision fine et le double-clic remet la valeur par défaut. Tout ça doit
   marcher de la même façon dans toutes les vues.
7. **Tout se fait au clavier.** Chaque action est dans la palette, avec son
   raccourci affiché. Les flèches et Entrée doivent suffire pour naviguer.
8. **Aucune modale bloquante.** La confirmation se remplace par l'annulation :
   on supprime tout de suite et on affiche « Annulé : … » avec Ctrl+Z.
9. **Le glisser-déposer partout.** Du navigateur vers le rack, la playlist, un
   insert ou un pad ; de l'OS vers la fenêtre ; d'une piste à une autre.
10. **Chaque nombre a un visuel.** Un paramètre qui a un effet audible doit avoir
    une courbe, une forme d'onde ou un indicateur qui bouge pendant la lecture.
11. **Des valeurs par défaut qui sonnent.** Un appareil inséré doit déjà faire
    quelque chose d'utile, et un projet neuf doit être prêt pour faire un beat.
12. **Des états vides qui guident.** Une vue vide dit en une phrase quoi faire,
    comme le font déjà le piano roll et la Session.

---

## 2. Vague 1 : les prochaines 2 à 3 heures

Ce sont les éléments au meilleur rapport valeur/effort, presque tous en S. Chacun
est indépendant sauf mention contraire.

| # | Élément | Pourquoi | Taille | Zone | Dépend de |
|---|---|---|---|---|---|
| V1-1 | **Barre d'aide au survol** : en bas de la fenêtre, nom, description et raccourci du widget survolé (descriptions du registre pour les appareils, `ParamInfo` pour les potards) | Plus grand gain de lisibilité pour les débutants ; règle 2 | S | ui (widgets.rs, app.rs) | – |
| V1-2 | **Sauvegarde automatique et reprise après crash** : écrit `~/Musique/Ponzik/.autosave/<nom>.ponzik` toutes les 2 min si le projet a changé ; au démarrage, propose de restaurer le fichier s'il est plus récent | Perdre un projet fait fuir un utilisateur pour de bon | S | project (io.rs), ui (compat.rs) | – |
| V1-3 | **Gamme du projet** : tonalité et mode (majeur, mineur, dorien, penta…) dans `Project` ; les lignes hors gamme sont grisées dans le piano roll ; option « coller à la gamme » | Base de tous les outils d'harmonie (Live 12, FL, Logic, Cubase) | S | project, ui (piano.rs) | – |
| V1-4 | **Accords en un clic** : un outil du piano roll pose triade, septième, sus ou power chord sur la gamme, avec renversement à la molette | Fonction préférée des débutants (FL chord stamps, Cubase Chord Pads) | S | ui (piano.rs) | V1-3 (sinon accords chromatiques) |
| V1-5 | **Humaniser et randomiser** : décalage de timing et de vélocité aléatoire réglable sur la sélection (palette + menu du piano roll) | Rend un beat moins mécanique en un geste | S | ui (piano.rs) | – |
| V1-6 | **Tap tempo et pré-compte** : bouton TAP (moyenne des 4 derniers appuis) ; pré-compte de 1 mesure avant l'enregistrement, au métronome | Indispensable pour enregistrer et caler un sample | S | engine (count-in dans le transport), ui | – |
| V1-7 | **Panneau d'historique** : liste des étiquettes d'annulation ; un clic revient à cet état | `History` stocke déjà les étiquettes ; Live, FL et Reaper l'ont tous | S | project (history.rs : itérateur des libellés, `goto(n)`), ui | – |
| V1-8 | **Glisser-déposer depuis le navigateur** : un WAV va dans un nouveau canal Sampler ou un clip audio de la playlist, un instrument va dans le rack, un effet dans un slot d'insert | Règle 9 ; c'est le geste naturel de FL comme d'Ableton | M | ui (app.rs : charge utile de glisser, cibles de dépôt) | – |
| V1-9 | **Mesure LUFS sur le master** : intégrée, court terme, momentanée, plus crête vraie (pondération K = 2 biquads, crête suréchantillonnée ×4) ; affichée à côté du VU master | Toute la musique publiée vise une cible LUFS ; Ozone et Pro-L l'affichent | S | dsp (pondération K), engine (meter.rs), ui (mixer.rs) | – |
| V1-10 | **Analyseur de spectre** : FFT radix-2 maison, courbe lissée en 1/6 d'octave derrière la courbe de l'EQ et dans un visuel du master | L'EQ sans spectre, c'est régler à l'aveugle (Pro-Q, Spectrum de Live) | M | dsp (fft.rs), engine (tampon d'analyse sans verrou), ui (visuals.rs) | – |
| V1-11 | **Make unique** et **renommer ou colorer par clic droit** les patterns et les clips | Le seul ⬜ qui reste dans la ligne Patterns ; geste FL de base | S | project, ui (playlist.rs) | – |
| V1-12 | **Modèles de projet** : « Enregistrer comme modèle » et « Nouveau depuis un modèle » dans la palette (dossier `Modèles/`) | Démarre une session en 2 s ; FL, Live et Studio One l'ont tous | S | ui (compat.rs, palette.rs) | – |
| V1-13 | **Normalisation et dithering à l'export** (TPDF en 16 bits, normalisation crête ou LUFS cible) | Un export propre sans avoir à passer par un autre logiciel | S | project (render.rs), dsp | V1-9 pour la cible LUFS |
| V1-14 | **Dupliquer la sélection de temps** (Ctrl+B sur la règle : tout le contenu de la plage est recopié juste après) | Façon la plus rapide d'allonger un arrangement (Live : Cmd+D) | S | ui (playlist.rs), project | – |

Les cinq à prendre en premier : **V1-1, V1-2, V1-3 puis V1-4, V1-7, V1-9.**

---

## 3. Vague 2 : les prochaines semaines

| # | Élément | Pourquoi | Taille | Zone | Dépend de |
|---|---|---|---|---|---|
| V2-1 | **Sidechain** : chaque effet de dynamique (Compresseur, Porte, Pompe, Multibande) peut écouter une autre piste | Le pompage de la house et de l'EDM ; rien ne le remplace | M | engine (`Effect::process_sidechain`, ordre de routage), ui | – |
| V2-2 | **Compensation de latence (PDC)** : `Effect::latency()`, retard compensé sur les pistes et les envois ; latence lue aussi sur les CLAP | Le Limiteur à anticipation et les CLAP décalent déjà le son | M | engine (mixer.rs), clap | – |
| V2-3 | **Pistes de groupe et VCA** : une piste de mixage en groupe repliable, et un fader VCA qui pilote plusieurs pistes sans faire passer l'audio par lui | Mixage d'une batterie ou de chœurs (toutes les STAN) | M | project, engine, ui (mixer.rs) | – |
| V2-4 | **Pistes de mixage dynamiques** : remplacer `TRACKS = 33` fixe par un nombre variable | FL 2025 monte à 500 pistes ; 33 limite les gros projets | M | engine, project, ui | – |
| V2-5 | **Macros d'appareil et racks** : chaîne d'effets ou d'instruments empilés, 8 macros mappables avec plage, enregistrée en preset | Cœur du « calme » d'Ableton : 8 potards au lieu de 80 | L | engine (rack d'effets), project, ui | – |
| V2-6 | **Modulateurs** : LFO, enveloppe, suiveur d'enveloppe, pas à pas, aléatoire, branchés par glisser sur n'importe quel paramètre, piste ou envoi compris, avec une profondeur par cible | Bitwig 5, Cubase 14 ; c'est ce qui donne du mouvement aux sons | L | engine (modulation par bloc), dsp (lfo.rs), ui | V2-5 conseillé |
| V2-7 | **MIDI learn** : clic droit, « Lier au contrôleur », bouger un CC ; table des liens enregistrée dans le projet | Le contrôleur MIDI branché ne sert aujourd'hui qu'aux notes | S | engine, project, ui | – |
| V2-8 | **Enregistrement d'automation en direct** (modes écriture, touch, latch) | Pour garder un geste fait à la main | M | engine, ui (automation.rs) | V2-7 utile |
| V2-9 | **Marqueurs de section et piste d'arrangement** : couplet, refrain…, que l'on peut glisser pour déplacer toute la section (Arranger de Studio One et Cubase) | Restructurer un morceau en un geste | M | project, ui (playlist.rs) | – |
| V2-10 | **Changements de tempo et de signature** : piste globale de tempo, rendu et séquence sur une carte de tempo | Indispensable dès qu'on sort de la boucle à 4/4 | M | engine (sequence.rs), project | – |
| V2-11 | **Warp des clips audio** : tempo d'origine détecté, étirement temporel (WSOLA pour la batterie, vocodeur de phase pour les sons tonals), transposition par clip, marqueurs de warp | Caler une boucle au tempo, la base de Live | L | dsp (stretch), engine (lecture des clips), ui | – |
| V2-12 | **Découpe aux transitoires vers pads ou MIDI** : le Découpeur exporte ses tranches dans un pattern de notes, ou un clip audio est découpé dans la playlist | Le flip de breaks (Slicex, Fruity Slicer 2, Simpler Slice) | M | engine (détection déjà dans le Découpeur), ui | – |
| V2-13 | **Pads de batterie** (rack de 16 pads, un échantillon et une chaîne par pad, chokes, glisser un WAV sur un pad) | Remplace FPC et Drum Rack | M | engine (instrument), ui | V1-8 |
| V2-14 | **Comping de prises** : boucle d'enregistrement, une lane par prise, balayer pour choisir les morceaux gardés (Reaper 7, Logic) | Voix et instruments enregistrés | L | engine (enregistrement), project, ui | – |
| V2-15 | **Freeze, bounce sur place, consolider** : rendre une piste ou un clip en audio, coller l'audio bouncé (Live 12.3) | Économiser le processeur et figer un son | M | project (render.rs hors ligne), ui | – |
| V2-16 | **Capture MIDI** : tampon permanent des notes jouées ; Ctrl+Maj+C crée le pattern même sans avoir enregistré | Ne plus perdre une bonne idée jouée en impro (Live, FL Score logger) | S | engine (anneau de notes), ui | – |
| V2-17 | **Transformations MIDI** : arpéger, strum, flam, legato, inverser, renverser, étirer, enchaîner (glue) | Outils du piano roll de FL et Transformations de Live 12 | M | ui (piano.rs), project | V1-3 |
| V2-18 | **Générateurs MIDI** : euclidien, rythme, motif sur la gamme, basse qui suit les accords (Bassline Generator de FL 2025) | Démarrer quand on bloque | M | project (génération pure), ui | V1-3, V1-4 |
| V2-19 | **Probabilité et répétitions par pas** (ratchets, condition 1:2, 3:4) dans le Channel Rack et le piano roll | Grooves vivants façon Elektron, Live, Bitwig Stepwise | M | engine (sequence.rs), project, ui | – |
| V2-20 | **Glisser des fichiers depuis l'OS** : XDND sur X11, `WM_DROPFILES` sur Win32, nouvel `Event::FileDrop` | Règle 9 ; on prend les samples dans le gestionnaire de fichiers | M | window, ui | V1-8 |
| V2-21 | **A/B d'appareil** : deux états par appareil et un bouton pour basculer (Live 12.3, plugins FabFilter) | Comparer les réglages honnêtement | S | project, ui (devices.rs) | – |
| V2-22 | **Favoris, tags et aperçu sonore dans le navigateur** : écoute au survol, au tempo, étoile, filtres par catégorie | Trouver un son vite, c'est ce qui fait rester sur FL | M | ui, engine (voix de pré-écoute) | – |
| V2-23 | **Panneau de clip** : gain, fondus réglables, inversion, transposition, boucle et couleur du clip sélectionné, en bas d'écran (Clip View de Live) | Règle 1 ; aujourd'hui les fondus sont fixes | M | project, engine, ui | – |
| V2-24 | **Mode de lancement et follow actions** dans la Session (trigger, gate, toggle, legato ; suivant, aléatoire, avec probabilité) | Live sur scène, jam génératif | M | engine (session), project, ui | – |
| V2-25 | **Export FLAC** (encodeur maison sans perte, prédiction LPC fixe) | Format libre et sans perte, faisable sans dépendance | M | project | – |
| V2-26 | **Onboarding** : première ouverture avec 3 étapes guidées (faire un beat, ajouter une basse, exporter) et des bulles sur les vrais widgets | Promesse « un beat en 30 s » | M | ui | V1-1 |

---

## 4. Vague 3 : chantiers longs

| # | Élément | Pourquoi | Taille | Zone | Dépend de |
|---|---|---|---|---|---|
| V3-1 | **Piste d'accords globale et instruments qui la suivent** : clips MIDI et audio transposés selon l'accord (Logic 11, Studio One, Cubase) | Changer l'harmonie de tout le morceau d'un geste | L | project, engine | V1-3, V2-17 |
| V3-2 | **Musiciens virtuels** : batteur, bassiste, claviériste qui jouent sur la piste d'accords, réglés par complexité et intensité (Session Players de Logic) | Aide énorme pour les débutants | L | project (génération), ui | V3-1 |
| V3-3 | **Brouillons (scratch pads)** : arrangement alternatif dans le même projet (Studio One), arrangements multiples (FL) | Tester une structure sans rien casser | M | project, ui | V2-9 |
| V3-4 | **Versions du projet** : instantanés nommés, comparaison et retour en arrière, avec la sauvegarde auto en continu | Remplace « projet_final_v7_VRAI.flp » | M | project, ui | V1-2 |
| V3-5 | **Correction de hauteur** : détection YIN, notes en blobs éditables (hauteur, vibrato, durée, formants), mode automatique accordé à la gamme | Melodyne et Newtone ; incontournable pour la voix | L | dsp (PSOLA), engine, ui | V1-3, V2-11 |
| V3-6 | **Édition spectrale** : spectrogramme d'un clip, gomme et atténuation au pinceau, réduction de bruit | Nettoyer des prises (Edison, RX, Spectral Layers) | L | dsp (STFT), ui | V1-10 |
| V3-7 | **Audio vers MIDI** (batterie, mélodie, harmonie) | Live le fait ; utile pour reprendre un sample | L | dsp | V3-5 |
| V3-8 | **Séparation de stems** locale | Live 12.3, FL, Studio One 7 et Cubase 15 l'ont tous. Sans dépendance, il faut un petit réseau écrit à la main, donc c'est le chantier le plus lourd | L | dsp | – |
| V3-9 | **Grille modulaire** : patch de modules (osc, filtre, enveloppe, logique, retard) pour faire ses instruments et effets (The Grid de Bitwig, Patcher de FL) | Puissance illimitée pour les sound designers | L | engine, ui | V2-6 |
| V3-10 | **MPE** : pression, glide et timbre par note, en entrée, dans le piano roll et dans les instruments natifs | Contrôleurs expressifs, Meld, Push 3 | L | midi, engine, project, ui | – |
| V3-11 | **Actions personnalisées et macros de commandes** : enchaîner des actions de la palette, les nommer, leur donner un raccourci (actions de Reaper) | Flux de travail des pros sans scripts | M | ui (palette.rs) | – |
| V3-12 | **Rendu par régions** : un fichier par marqueur de section, des modèles de nommage (Reaper) | Exporter un EP ou des boucles d'un coup | S | project (render.rs) | V2-9 |
| V3-13 | **Ableton Link et horloge MIDI** (entrée et sortie) | Jouer avec d'autres machines | M | engine, midi (réseau UDP maison) | – |
| V3-14 | **Hôte VST3 et LV2**, plugins isolés dans un processus séparé | Accès à tout l'écosystème de plugins | L | nouveau crate | – |
| V3-15 | **Import DAWproject** (et `.als` partiel) | Arriver avec ses projets d'ailleurs | L | project | – |
| V3-16 | **Enregistrement du master (resampling)** depuis la barre de transport (Bitwig 5.3) | Garder un jam improvisé | S | engine, ui | – |
| V3-17 | **Recherche partout** : la palette cherche aussi dans les presets, les samples, les canaux, les pistes et les marqueurs, et y emmène | Règle 7 | M | ui (palette.rs) | V2-22 |
| V3-18 | **Fenêtres détachables, multi-écran** | Pros sur deux écrans | L | window, ui | – |

---

## 5. Plugins natifs à créer

Les noms sont fonctionnels ; l'inspiration est entre parenthèses et ne doit pas
apparaître dans le produit.

| Plugin | Fonction | Inspiré de | Vague |
|---|---|---|---|
| **Spectre** | Analyseur FFT, comparaison de deux pistes, mesure LUFS | Pro-Q (analyseur), SPAN, Youlean | 1 (V1-9, V1-10) |
| **EQ dynamique** | Bandes de l'EQ actuel avec seuil, en mode dynamique ou spectral (seulement les fréquences qui dépassent), solo de bande, dessin de courbe à main levée | Pro-Q 4 (Spectral Dynamics, EQ Sketch) | 2 |
| **Pompe sidechain** | La Pompe actuelle avec une courbe dessinée et une entrée externe ou une détection de kick | Kickstart, LFOTool | 2 (V2-1) |
| **Hachoir de volume et de hauteur** | Deux courbes dessinées par mesure (volume, position de lecture), retours en arrière, ralentis, presets de rythme | Gross Beat | 2 |
| **Compresseur de bus** | Colle et caractère : modes propre, opto, VCA, mesure de réduction de gain avec historique déroulant | Pro-C 2, Glue | 2 |
| **Limiteur de mastering** | Plusieurs algorithmes (transparent, punchy, agressif), crête vraie, cible LUFS, historique du niveau | Pro-L 2, Emphasis | 2 |
| **Assistant de mastering** | Analyse le mix, propose une chaîne EQ, multibande, image, limiteur vers une cible de genre ou un morceau de référence, avec 4 potards | Ozone Master Assistant | 3 |
| **Multibande extrême** | Mode « tout écraser » du Multibande actuel avec préréglage 1 potard et visuel par bande | OTT | 1 (preset) |
| **Réverbe algorithmique** | Modes salle, plaque, nuage de diffusion, modulation riche, très longues queues | ValhallaVintageVerb, Supermassive | 2 |
| **Délai à caractère** | Bande, analogique, ping-pong, saturation dans la boucle, wow et flutter, ducking | EchoBoy, Valhalla Delay | 2 |
| **Saturation à caractère** | Lampe, bande, transistor, redresseur, en multibande, avec un mix | Decapitator, Saturn 2 | 2 |
| **Filtre animé** | Filtre à enveloppe, LFO dessiné et pas à pas, modes vowel et comb | FilterFreak, MicroShift | 2 |
| **Vocodeur** | 16 à 32 bandes, porteuse interne ou sidechain, formants | Vocodex, Vocoder de Live | 2 (V2-1) |
| **Correcteur de hauteur** | Accordage automatique en temps réel sur la gamme, vitesse réglable (effet « T-Pain » à 0 ms) | Auto-Tune, Pitcher, Melodyne | 3 (V3-5) |
| **Synthé tables d'ondes 2** | Ponzik Wave étendu : 3 oscillateurs + sub + bruit, modes spectral, granulaire et multi-échantillon, matrice de modulation, distorsion de phase, import de tables | Serum 2, Vital | 3 (V2-6) |
| **Synthé de batterie modulaire** | Kick, caisse, charleston par couches (transitoire + corps + bruit), une famille de presets par machine | Bitwig v8, v9 et v0 | 2 |
| **Bruits et texture** | Craquements de vinyle, souffle de bande, perte de bits par bloc, pleurage | RC-20 | 2 |
| **Arpégiateur et effets MIDI** | Arpège, accords, gamme, écho de notes, aléatoire, en appareils MIDI placés avant l'instrument | Effets MIDI de Live | 2 (V2-17) |

---

## 6. Catalogue des absences par domaine

Ce qui manque à Ponzik et ce qui le couvre dans le backlog. Les fonctions déjà
présentes dans Ponzik ne figurent pas ici.

- **Composition** : gamme (V1-3), accords (V1-4), humanisation (V1-5),
  transformations (V2-17), générateurs (V2-18), probabilité (V2-19), capture
  (V2-16), piste d'accords (V3-1), musiciens virtuels (V3-2), MPE (V3-10),
  pattern sequencer polyphonique (Cubase 14, V2-19), micro-tonalité (hors
  vagues).
- **Arrangement** : sections (V2-9), tempo et signature (V2-10), brouillons et
  arrangements multiples (V3-3), duplication de plage (V1-14), modes de
  lancement et follow actions (V2-24), performance mode de FL (dans V2-24).
- **Audio** : warp et stretch (V2-11), découpe (V2-12), comping (V2-14), freeze
  et bounce (V2-15), panneau de clip et fondus (V2-23), correction de hauteur
  (V3-5), spectral (V3-6), audio vers MIDI (V3-7), stems (V3-8), resampling
  (V3-16).
- **Mixage** : LUFS (V1-9), spectre (V1-10), sidechain (V2-1), PDC (V2-2),
  groupes et VCA (V2-3), pistes dynamiques (V2-4), mid/side dans l'EQ et le
  compresseur (V2, dans EQ dynamique), cue et pré-écoute (hors vagues).
- **Sound design** : macros et racks (V2-5), modulateurs (V2-6), A/B (V2-21),
  grille modulaire (V3-9).
- **Workflow et confort** : aide au survol (V1-1), sauvegarde auto (V1-2),
  historique (V1-7), glisser-déposer (V1-8, V2-20), make unique (V1-11), modèles
  (V1-12), MIDI learn (V2-7), automation enregistrée (V2-8), navigateur avec
  favoris et pré-écoute (V2-22), onboarding (V2-26), versions (V3-4), actions
  (V3-11), recherche partout (V3-17), outil intelligent (un outil qui pose,
  déplace et redimensionne selon la zone survolée : à vérifier sur chaque vue
  pendant V1).
- **Live** : follow actions (V2-24), Link et horloge MIDI (V3-13), looper (hors
  vagues).
- **Export et partage** : normalisation et dithering (V1-13), FLAC (V2-25), rendu
  par régions (V3-12), DAWproject (V3-15). MP3 et OGG demandent un encodeur
  maison : à faire après FLAC.

---

## Sources

- Ableton Live 12.3 : https://www.ableton.com/en/blog/live-12-3-is-coming/ · https://cdm.link/live-12-3-arrives/ · https://www.musicradar.com/music-tech/stem-separation-splice-integration-ableton-is-playing-catch-up-with-live-12-3-but-its-best-new-features-are-the-ones-hidden-beyond-the-headlines
- Info View d'Ableton : https://www.ableton.com/en/live-manual/12/first-steps/
- FL Studio 2025 : https://www.image-line.com/fl-studio-news/fl-studio-2025-whats-new-2 · https://www.image-line.com/fl-studio-news/history/fl-studio-2025-2 · https://www.attackmagazine.com/news/fl-studio-2025-adds-ai-support-and-workflow-improvements-in-annual-update/
- Bitwig Studio 5 et 5.3 : https://musictech.net/news/gear/bitwig-studio-5-update-new-features/ · https://synthanatomy.com/2025/02/bitwig-studio-5-3-new-drum-devices-sequencer-background-recording-and-winter-sale.html
- Logic Pro 11 : https://apple.com/newsroom/2024/05/logic-pro-takes-music-making-to-the-next-level-with-new-ai-features · https://www.soundonsound.com/techniques/logic-pro-session-players
- Studio One Pro 7 : https://prnewswire.com/news-releases/presonus-introduces-the-daw-for-all-studio-one-pro-7-with-all-new-innovative-production-tools-302270757.html · https://www.soundonsound.com/reviews/presonus-studio-one-pro-7
- Cubase 14 : https://www.musicradar.com/music-tech/daws/make-composing-producing-and-mixing-more-inspirational-than-ever-steinberg-launches-cubase-14-with-new-pattern-editor-drum-machine-effects-plugins-modulators-and-more
- Reaper 7 : https://www.reaper.fm/userguide/WhatsNewReaper7Summary_r2.pdf · https://www.soundonsound.com/news/cockos-launch-reaper-7
- Serum 2 : https://www.kvraudio.com/news/xfer-records-releases-serum-2---free-update-63286 · https://splice.com/blog/serum-2-advanced-features/
- FabFilter Pro-Q 4 : https://www.fabfilter.com/news/1733994000/fabfilter-releases-pro-q-4-equalizer-plug-in · https://www.soundonsound.com/reviews/fabfilter-pro-q-4
- iZotope Ozone 11 : https://izotope.com/en/products/ozone/features/also-includes/master-assistant-view.html · https://www.soundonsound.com/reviews/izotope-ozone-11-advanced
- Melodyne 5 : https://www.pluginboutique.com/products/6442-Melodyne-5-Studio
