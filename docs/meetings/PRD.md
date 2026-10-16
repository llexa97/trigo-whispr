# PRD — Enregistrement de réunions (V1)

## 1. Problème

Trigo Whispr transcrit aujourd'hui des dictées courtes (raccourci → micro → texte collé). Les utilisateurs passent une grande partie de leur journée en réunion (Teams, Zoom, Slack, Webex…) et n'ont aucun moyen simple, **local et privé**, d'en garder une transcription. Les solutions existantes envoient l'audio dans le cloud ou ajoutent un bot dans l'appel.

## 2. Objectif

Permettre d'enregistrer et de transcrire **localement** une réunion en captant **le micro (« Moi ») et le son du système (« Autres »)**, sans bot, avec une **détection automatique** de la réunion qui propose de lancer l'enregistrement (à la manière de Notion AI Meeting Notes).

## 3. Périmètre V1

| Dans le périmètre                                                       | Hors périmètre (V2+)                                           |
| ----------------------------------------------------------------------- | -------------------------------------------------------------- |
| **Windows 10/11 (cible principale, MVP)** puis macOS 14.2+ (secondaire) | Linux (le code compile, la fonctionnalité est masquée)         |
| Capture micro + son système en deux pistes alignées                     | Diarisation par participant (sherpa-onnx / VBx)                |
| Démarrage/arrêt manuel (tray, raccourci, UI)                            | Intégration calendrier (Google / Outlook)                      |
| Détection auto via « quelle app utilise le micro » + popup              | Distinction fiable de Google Meet/Teams web dans un navigateur |
| Transcription locale post-réunion (moteur existant `transcribe-cpp`)    | Transcription en direct pendant la réunion                     |
| Attribution « Moi / Autres » + suppression de l'écho                    | Résumé / Q&A par LLM, templates de notes                       |
| Section « Réunions » : liste, détail, lecture, export Markdown          | Partage, sync cloud, MCP                                       |
| Reprise après crash, rétention des fichiers                             | Enregistrement vidéo / écran                                   |

## 4. Utilisateurs & scénarios

- **S1 — Démarrage manuel** : je rejoins un Zoom, je clique « Enregistrer la réunion » dans le tray (ou raccourci). Un indicateur montre que l'enregistrement est actif. Je clique « Arrêter » à la fin.
- **S2 — Détection auto** : Teams prend le micro → une popup « Réunion Teams détectée — Enregistrer / Ignorer » apparaît. Quand Teams relâche le micro, l'enregistrement s'arrête (ou on me le propose).
- **S3 — Consultation** : après l'arrêt, la transcription se fait en tâche de fond ; je retrouve la réunion dans « Réunions » avec un transcript horodaté « Moi / Autres », je peux réécouter, copier, exporter en Markdown, renommer, supprimer.
- **S4 — Dictée pendant une réunion** : la dictée classique continue de fonctionner pendant un enregistrement de réunion.
- **S5 — Crash** : si l'app plante pendant une réunion de 2 h, l'audio déjà capté est récupéré au redémarrage.

## 5. Exigences fonctionnelles

1. **F1 Capture** : deux pistes séparées (micro, système), alignées sur l'horloge réelle (silence inséré si une piste ne reçoit rien), écrites **au fil de l'eau** sur disque.
2. **F2 Dégradation** : si le son système est indisponible (macOS < 14.2, permission refusée, build non signé, sortie muette), l'enregistrement continue en micro seul avec un avertissement visible.
3. **F3 Démarrage manuel** : tray, raccourci global configurable, bouton dans la section Réunions, flag CLI `--toggle-meeting`.
4. **F4 Détection** : surveillance périodique de l'app qui utilise le micro, comparée à une liste d'apps de visio activables ; popup Enregistrer / Ignorer ; option « démarrer automatiquement sans demander » ; arrêt auto (ou proposé) quand l'app relâche le micro.
5. **F5 Transcription** : post-réunion, par piste, avec le modèle sélectionné ; horodatage par segment ; fusion chronologique « Moi / Autres » ; suppression de l'écho micro (sans casque) ; progression visible ; annulable ; la dictée reste prioritaire.
6. **F6 Consultation** : liste des réunions (titre, date, durée, app source, statut), détail avec transcript, lecteur audio, copie, export Markdown, renommage, suppression.
7. **F7 Robustesse** : manifeste sur disque + reprise au démarrage ; empêcher la mise en veille pendant l'enregistrement ; changement de périphérique en cours de réunion géré.
8. **F8 Réglages** : activer la détection, liste des apps, démarrage auto, arrêt auto, capture du son système, rétention de l'audio des réunions.

## 6. Exigences non fonctionnelles

- **100 % local** : aucun audio ni texte ne sort de la machine.
- **Vie privée** : l'utilisateur est seul responsable d'informer les participants ; un rappel est affiché à la première utilisation.
- **Performance** : la capture ne doit pas dégrader la dictée ni l'appel (CPU < 5 % en capture sur une machine récente).
- **Stockage** : ~115 Mo/h/piste en WAV 16 kHz mono i16 ; nettoyage selon la rétention.
- **i18n** : toutes les chaînes via i18next (au minimum `en` et `fr`).
- **Linux** : compile sans erreur, fonctionnalité masquée.

## 7. Contraintes techniques clés

- **Windows d'abord** : l'app vise surtout Windows. Le MVP (T1→T7) doit être complet et recetté sur Windows avant d'attaquer les tâches macOS (T8→T10).
- **macOS** : capture du son système via **Core Audio process tap** (14.2+), symboles résolus à l'exécution (`dlsym`) car `minimumSystemVersion` = 10.15. Le tap ne renvoie **que du silence** si l'app n'a pas une signature stable → script d'auto-signature pour le dev, `NSAudioCaptureUsageDescription` dans `Info.plist`.
- **Windows** : capture du son système via **process loopback WASAPI excluant Trigo** (Windows 10 build 20348+ / Windows 11), avec repli sur l'**endpoint loopback** (flux d'entrée cpal sur le périphérique de sortie, supporté par `handy-cpal 0.18.2`). Détection via la clé de registre `CapabilityAccessManager\ConsentStore\microphone` (sémantique de `LastUsedTimeStop == 0` à valider).
- **Base de code** : on **vendore** (copie avec attribution MIT) les fichiers utiles de [fly-on-the-wall](https://github.com/swarnavspujari/fly-on-the-wall) : `crates/fly-audio/src/{cpal_backend,coreaudio_tap,mix,tap_messages,win_volume}.rs`, `crates/fly-core/src/crosstalk.rs`, idées de `crates/fly-storage/src/recovery.rs`, `scripts/macos-selfsign.sh`. On **ne reprend pas** `fly-asr` (sidecar whisper-cli) : on réutilise `transcribe-cpp` et le `Chunker` existants.
- **Conflits avec l'existant** : le réglage `mute_while_recording` coupe la sortie audio pendant la dictée → doit être neutralisé pendant une réunion ; les sons de feedback de Trigo ne doivent pas polluer la piste système.

## 8. Métriques de succès

- Une réunion Teams et une réunion Zoom de 30 min sont enregistrées et transcrites sur Windows 11 (puis macOS), avec « Moi / Autres » correctement attribués (> 90 % des segments).
- Détection : popup affichée en < 10 s après la prise du micro par Teams/Zoom ; 0 faux positif sur une journée d'usage hors réunion (dictée Trigo incluse).
- 0 réunion perdue lors d'un crash simulé.

## 9. Risques

| Risque                                                                         | Mitigation                                                                   |
| ------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| Tap macOS silencieux (signature / TCC)                                         | Script d'auto-signature, preflight de consentement, avertissement UI         |
| Attribution micro→process imprécise sur macOS (changement de micro, Bluetooth) | Validation sur matériel réel, fallback « micro actif + app de visio lancée » |
| Sémantique de `LastUsedTimeStop == 0` non documentée (Windows)                 | Valider sur machine réelle avant de finaliser l'issue Windows                |
| Transcription longue bloquant la dictée                                        | Découpage par morceaux, dictée prioritaire, annulation                       |
| Feature freeze hérité d'upstream (AGENTS.md)                                   | Décision du mainteneur du fork                                               |

## 10. Liste de tâches

### Tâches de code (une issue GitHub chacune)

**MVP Windows**

| #       | Tâche                                                                     | Dépend de |
| ------- | ------------------------------------------------------------------------- | --------- |
| T1 · #3 | Socle de capture double piste : micro + loopback WASAPI (Windows)         | —         |
| T2 · #4 | `MeetingManager` : cycle de vie, stockage, reprise après crash, rétention | T1        |
| T3 · #5 | Pipeline de transcription « Moi / Autres » + suppression de l'écho        | T2        |
| T4 · #6 | Démarrage manuel : tray, raccourci, CLI, indicateur                       | T2        |
| T5 · #7 | UI : section « Réunions » (liste, détail, lecture, export)                | T2, T3    |
| T6 · #8 | Détection : moteur commun, popup, réglages                                | T4        |
| T7 · #9 | Détection Windows : ConsentStore microphone                               | T6        |

**macOS (secondaire)**

| #         | Tâche                                                        | Dépend de |
| --------- | ------------------------------------------------------------ | --------- |
| T8 · #10  | macOS : permissions, entitlements et script d'auto-signature | —         |
| T9 · #11  | macOS : capture du son système par Core Audio process tap    | T1, T8    |
| T10 · #12 | Détection macOS : process audio utilisant le micro           | T6        |

### Tâches hors code

- [ ] Valider sur une vraie machine Windows 10 et 11 la sémantique de `LastUsedTimeStop` (prérequis de T7).
- [ ] Valider sur macOS l'attribution micro→bundle ID avec Teams, Zoom, et un micro Bluetooth (prérequis de T10).
- [ ] Recette complète sur Windows 10 et 11 : Teams + Zoom, avec et sans casque (puis macOS).
- [ ] Signature de code Windows (certificat) pour éviter SmartScreen à la distribution.
- [ ] Décider de la levée du feature freeze sur le fork et mettre à jour `AGENTS.md`.
- [ ] Mettre à jour `README.md` / `BUILD.md` (permissions, signature, limites).
- [ ] Obtenir un certificat Apple Developer ID pour distribuer une build où le son système fonctionne hors dev.
- [ ] Rédiger le message de rappel légal (consentement des participants).
