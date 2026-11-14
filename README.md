# xbox-dongle

Driver en espace utilisateur pour l'**adaptateur sans fil Xbox** (le dongle USB
Microsoft) sur macOS. Il pilote le dongle, appaire et connecte les manettes Xbox,
puis les présente à macOS comme de vraies manettes Xbox, reconnues par le
framework GameController (jeux Mac natifs), SDL, Steam et les navigateurs.

Écrit en Rust, sans extension noyau ni DriverKit, et sans compte développeur
Apple payant — au prix d'une sécurité système réduite (voir
[Prérequis](#prérequis-sécurité)).

## État

Testé sur un Mac M4 (macOS 27.2), dongle « slim » `045e:02fe`, manette Xbox
Series (firmware 5.23.6.0).

| Fonction | État |
|---|---|
| Chargement du firmware (à froid et à chaud) | ✅ |
| Appairage, reconnexion automatique, voyants | ✅ |
| Entrées : 15 boutons, croix, gâchettes, sticks | ✅ |
| Reconnu par GameController comme manette Xbox | ✅ |
| Vibrations (poignées et gâchettes) | ✅ |
| Bouton Guide transmis aux applications | ❌ réservé par macOS |
| Chiffrement du lien radio | ❌ non implémenté (pas nécessaire pour cette manette) |
| Plusieurs manettes simultanées | ⚠️ prévu dans le code, jamais essayé |
| Casque audio, mise à jour de firmware manette | ❌ |

Dongles reconnus : `045e:02e6` (original), `045e:02fe` (slim), `045e:02f9` et
`045e:091e` (intégrés). Seul le `02fe` a été essayé.

## Prérequis (sécurité)

La manette virtuelle est créée avec l'API `IOHIDUserDevice`, qui exige
l'entitlement restreint `com.apple.developer.hid.virtual.device`. Sans profil de
provisionnement Apple, un binaire signé avec cet entitlement est tué au
lancement, sauf si **SIP est désactivé et AMFI assoupli**.

Sur un Mac Apple Silicon : éteindre, maintenir le bouton d'alimentation →
**Options** → **Utilitaires → Terminal**, puis :

```bash
csrutil disable
nvram boot-args="amfi_get_out_of_my_way=1"
```

et redémarrer. Vérifier ensuite avec `csrutil status` et `nvram boot-args`.

> ⚠️ C'est une baisse de sécurité pour toute la machine : SIP ne protège plus le
> système, et AMFI ne vérifie plus les signatures ni les entitlements d'aucun
> programme. Les apps iPhone/iPad sur Mac peuvent cesser de fonctionner.

Pour revenir en arrière (même Terminal de récupération) :

```bash
nvram -d boot-args
csrutil enable
```

Sans ces réglages, le driver fonctionne quand même (appairage, entrées dans le
journal) avec une compilation `cargo build` non signée, mais aucune manette
n'apparaît dans macOS.

Autres prérequis : Rust (édition 2024), les outils en ligne de commande Xcode
(`codesign`), `curl`.

## Installation

1. **Récupérer le firmware** du dongle depuis le Microsoft Update Catalog.
   Il n'est pas redistribué avec ce projet et reste soumis aux
   [conditions d'utilisation de Microsoft](https://www.microsoft.com/en-us/legal/terms-of-use).
   Le modèle s'obtient avec `cargo run -- list`.

   ```bash
   ./scripts/fetch-firmware.sh 02fe
   ```

2. **Installer l'app** de barre des menus :

   ```bash
   ./scripts/install-app.sh
   ```

   Le script construit `Xbox Dongle.app` (l'app Swift et le driver signé avec
   l'entitlement, intégré au bundle), l'installe dans `/Applications`, copie le
   firmware dans `~/Library/Application Support/xbox-dongle/firmware/` et lance
   l'app. Il retire l'agent launchd s'il était installé.

   L'app lance le driver, le relance s'il s'arrête, et affiche son état dans la
   barre des menus :

   | Icône | État |
   |---|---|
   | manette estompée | en attente de l'adaptateur |
   | manette (contour) | adaptateur prêt, aucune manette |
   | manette pleine | au moins une manette connectée |
   | clignotante | appairage en cours |
   | triangle | erreur (détail dans le menu) |

   Le menu liste les manettes et leur batterie, et propose « Appairer une
   manette », « Télécharger le firmware… » s'il manque, « Ouvrir à l'ouverture
   de session », le journal et le redémarrage du driver.

   Journal : `~/Library/Logs/xbox-dongle.log`.

   *Variante sans app* : `./scripts/install-agent.sh` installe le driver seul
   comme agent launchd (démarré à l'ouverture de session, sans icône). Ne pas
   utiliser les deux en même temps.

3. **Appairer une manette** : appuyer sur le bouton du dongle (sa LED clignote),
   puis maintenir le bouton d'appairage de la manette jusqu'à ce que son logo
   clignote vite. Ensuite, la manette se reconnecte seule à l'allumage.

   Si la manette a été appairée au Mac en Bluetooth, l'oublier dans
   Réglages Système → Bluetooth, sinon elle s'y reconnectera plutôt qu'au dongle.

Désinstallation : quitter l'app et supprimer `/Applications/Xbox Dongle.app`
(et `~/Library/Application Support/xbox-dongle`), ou
`./scripts/uninstall-agent.sh` pour la variante agent.

## Utilisation manuelle

```bash
./scripts/build-signed.sh            # compile en release et signe
./target/release/xbox-dongle list    # détecte le dongle
./target/release/xbox-dongle run firmware/xbox_dongle_02fe.bin
```

| Commande | Rôle |
|---|---|
| `list` | Affiche le dongle branché et le firmware attendu |
| `load-firmware <fw>` | Charge seulement le firmware |
| `run <fw>` | Pilote le dongle jusqu'à son débranchement |
| `serve <fw>` | Comme `run`, mais attend et relance indéfiniment (utilisé par l'app et l'agent) |

Avec `--json` en premier argument, le driver écrit son état en JSON sur la
sortie standard, une ligne par événement, et lit des commandes sur l'entrée
standard (`pair`). C'est ainsi que l'app communique avec lui, voir
[src/status.rs](src/status.rs).

`<fw>` est un fichier firmware ou un dossier contenant des
`xbox_dongle_<pid>.bin` (le bon est choisi selon le dongle).

Journal détaillé : `RUST_LOG=xbox_dongle=debug` (liste de contrôle des boutons,
vibrations, trames de commande) ou `=trace` (toutes les trames radio et GIP).

Ne pas lancer `run` pendant que l'app ou l'agent tourne : un seul programme
peut piloter le dongle. Quitter l'app depuis son menu, ou arrêter l'agent avec
`launchctl bootout gui/$(id -u)/com.github.xbox-dongle`.

## Fonctionnement

```
manette ──radio──▶ dongle (puce MT76 + firmware Microsoft)
                      │ USB (requêtes vendeur, bulk)
                      ▼
               xbox-dongle (nusb)  ◀──JSON──▶  Xbox Dongle.app (barre des menus)
                 ├─ mt76.rs    firmware, radio, balise, clients 802.11
                 ├─ dongle.rs  boucle d'événements, appairage, clients
                 ├─ gip.rs     protocole GIP (annonce, identification, entrées, vibrations)
                 ├─ hid.rs     manette virtuelle IOHIDUserDevice
                 └─ status.rs  état JSON et commandes pour l'app
                      │
                      ▼
        macOS : GameController, SDL, Steam, navigateurs
```

- **Dongle.** Puce MediaTek MT76 (`0x7613`) sur un lien 802.11 propriétaire à
  5 GHz. Le driver lui envoie son firmware, configure la radio, émet la balise
  d'appairage et associe les manettes (jusqu'à 16).
- **GIP.** Les manettes parlent le *Gaming Input Protocol*, documenté par
  Microsoft dans [MS-GIPUSB](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-gipusb/e7c90904-5e21-426e-b9ad-d82adeee0dbc).
- **Manette virtuelle.** Elle imite exactement une manette Xbox Series en
  Bluetooth LE (`045e:0b13`) : même descripteur HID (relevé sur une vraie
  manette avec `ioreg`), mêmes propriétés. macOS crée lui-même ses manettes
  Bluetooth LE comme périphériques virtuels, et sa personnalité GameController
  accepte les périphériques virtuels pour ce modèle (`GCIOMatchVirtual`) : la
  manette est donc reconnue comme une Xbox. Les vibrations arrivent par le
  rapport de sortie n°3, de même format que la commande GIP de vibration.

## Écarts notables avec xone

- Après une demande d'appairage, l'appairage reste ouvert jusqu'à l'association
  de la manette (xone le ferme aussitôt, ce qui laissait parfois la manette
  bloquée en recherche).
- Relance à chaud du firmware : méthode de xow (réinitialisation puis
  rechargement) ; le dongle `02fe` signale la réinitialisation par `0` et non
  `0x80000000`.
- Bouton Share : octet à l'index 18 du rapport d'entrée, avec ou sans les
  compteurs de latence en fin de rapport.

## Crédits et licence

GPL-2.0-or-later. Le code radio et GIP est porté de
[xone](https://github.com/dlundqvist/xone) et [xow](https://github.com/medusalix/xow)
(Severin von Wnuck-Lipinski et contributeurs, GPL-2.0-or-later), avec l'aide de
la spécification MS-GIPUSB de Microsoft.

Le firmware du dongle appartient à Microsoft et n'est pas distribué ici.
