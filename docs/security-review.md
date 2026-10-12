# Revue de sécurité de PdfCraft (v0.6.0)

> **Last reviewed:** 2026-10-12 · **Last updated:** 2026-10-12 · **Change:** major (première revue) · **Périmètre :** commit `bbbe600`, ~324 000 lignes de Rust, 30 crates + `apps/` + CI

Revue en boîte blanche, centrée sur ce qu'un attaquant peut faire avec un PDF piégé, un agent
automatisé ou la chaîne de build. Chaque constat marqué **confirmé** a été reproduit avec un test
ou par l'exécution de `pdfcraft-cli`. Les autres viennent de la lecture du code.

## Synthèse

Le socle est bien plus mûr que la moyenne des projets à ce stade : Rust sans `unsafe`
(`unsafe_code = "forbid"`), clippy interdit `unwrap`/`panic` dans les 30 crates, plafonds de
décompression, moteur JavaScript isolé et borné, liens issus des documents filtrés et soumis à
confirmation, canal de contrôle authentifié, réécriture complète forcée après caviardage.

Les vrais problèmes se trouvent dans la **logique métier de sécurité** : la validation des
signatures et le caviardage. Ces deux fonctions servent justement à établir la confiance.

| # | Gravité | Constat | État |
|---|---|---|---|
| 1 | **Élevée** | Une mise à jour incrémentale qui ajoute un *nouveau* catalogue est classée « remplissage de formulaire » : le contenu affiché d'un document signé change et la signature reste **Valide** | confirmé (PoC) |
| 2 | **Moyenne** | Le caviardage laisse les vignettes de page (`/Thumb`), qui sont des images du contenu d'origine | confirmé (PoC) |
| 3 | **Moyenne** | La validité du certificat est jugée à l'heure *déclarée par le signataire*, et la révocation n'est jamais vérifiée en ligne | code |
| 4 | **Moyenne** | La clé de fichier AES-256 et les sels viennent de `RandomState` (SipHash), pas d'un CSPRNG | code |
| 5 | Moyenne | Le serveur MCP n'a aucun confinement par défaut (`--root` est optionnel). Un agent manipulé par un PDF peut lire et écrire partout, et signer avec les identités du magasin Windows | code |
| 6 | Faible | Les actions GitHub tierces sont épinglées par tag mutable dans les jobs qui détiennent les secrets de signature | code |
| 7 | Faible | Canal de contrôle : lignes de longueur illimitée et connexion maintenue avant authentification. Un processus local peut épuiser la mémoire | code |
| 8 | Info | Les algorithmes RC4-40/RC4-128 restent proposés pour chiffrer. L'IV AES est déterministe | code |

---

## 1. Contournement de la validation des signatures par ajout d'un catalogue (élevée, confirmé)

**Où :** `crates/sign/src/pdf.rs`, `change_kind()` (branche `Some(b"Catalog")`, cas `None => "form fill"`)
et `classify_changes()`, qui ne compare jamais le `/Root` du trailer.

Une révision ajoutée après la signature peut créer un **nouvel** objet catalogue (nouveau
numéro) et faire pointer le `/Root` du trailer vers lui. L'objet n'existait pas dans la version
signée (`before == None`), donc `change_kind` le range dans « form fill ». Ce type est autorisé
pour une signature d'approbation et pour une certification P=2 ou P=3. Le nouveau catalogue peut
réécrire librement `/OCProperties` (calques visibles), `/OpenAction`, `/Names`, `/AA`, `/AcroForm`…
Modifier le catalogue *existant* est en revanche bien refusé (« document structure »).

**Scénario (attaque « shadow » / ISA, Mainka et al.) :** l'attaquant fait signer un PDF dont la page
contient deux calques : « Pay 100 EUR » (visible) et « Pay 999 999 EUR » (masqué). Après
signature, il ajoute un catalogue qui inverse les calques.

**Résultat mesuré (PoC) :**

```
certify=None    new_catalog=false -> Invalid Disallowed(["document structure"])
certify=None    new_catalog=true  -> Valid   Allowed(["form fill"])      <-- contournement
certify=Some(1) new_catalog=true  -> Invalid Disallowed(["form fill"])
certify=Some(2) new_catalog=true  -> Valid   Allowed(["form fill"])      <-- contournement
certify=Some(3) new_catalog=true  -> Valid   Allowed(["form fill"])      <-- contournement
```

Le rendu `page_render` et `text_extract` du fichier falsifié donnent « Pay 999 999 EUR »,
alors que la signature est annoncée valide, avec la mention « changes are permitted (form fill) ».

**Correctif proposé :**
- Comparer le `/Root` (et `/Info`, `/Encrypt`) du trailer de chaque révision ultérieure avec
  ceux de la révision signée. Tout changement de `/Root` devient « document structure ».
- Un catalogue sans `before` ne doit jamais être « form fill ». Plus généralement, un objet neuf
  ne devrait être accepté que s'il est atteint depuis un objet modifié de façon autorisée. Le
  fallback `before.is_none() => "form fill"` est trop permissif.
- Ajouter à `crates/sign/tests/pdf.rs` un test `assert_refused_at_every_level` pour ce cas, et les
  variantes : nouveau `/Pages`, nouveau `/OCProperties` atteint via un nouveau catalogue,
  `/OpenAction` JavaScript.

<details><summary>Reproduction (test d'intégration jetable pour <code>crates/sign/tests/</code>)</summary>

Fixture : une page dont le contenu est
`/OC /A BDC … (Pay 100 EUR) Tj … EMC /OC /B BDC … (Pay 999 999 EUR) Tj … EMC`, avec deux OCG et
`/OCProperties << /OCGs [7 0 R 8 0 R] /D << /OFF [8 0 R] >> >>`. On la signe avec
`tests/data/rsa-aes.p12`, puis :

```rust
let root = doc.root().unwrap();
let mut cat = doc.get(root).as_dict().unwrap().clone();
// /OCProperties << /OCGs [7 0 R 8 0 R] /D << /OFF [7 0 R] >> >>
cat.set(b"OCProperties".to_vec(), Object::Dict(swapped));
let r = doc.add(Object::Dict(cat));
doc.trailer_mut().set(b"Root".to_vec(), Object::Ref(r));
let edited = write_incremental(&doc, &SaveOptions { object_streams: false, ..Default::default() })?;
// signatures(&open(&edited), &edited, &trust) -> Status::Valid, Modification::Allowed(["form fill"])
```
</details>

## 2. Le caviardage laisse les vignettes de page (moyenne, confirmé)

**Où :** `crates/redact/src/lib.rs` `apply()`. Aucune référence à `/Thumb` dans `crates/redact`
(seul `crates/optimize` les supprime).

Après `apply()` et une réécriture complète, le texte caviardé a bien disparu, mais l'image
`/Thumb` de la page, rendue avant caviardage, reste dans le fichier. PoC : `SECRET-NAME present:
false, thumbnail present: true`. Selon le producteur, la vignette est assez lisible pour retrouver
des montants, des signatures ou des photos.

**Correctif :** supprimer `/Thumb` de chaque page caviardée dans `apply()` (et dans `sanitize`).
Ajouter `/Thumb` à la vérification de résidus. Vérifier aussi les autres copies du contenu :
`/PieceInfo` propres à une page et versions alternatives des images (`/Alternates`).

## 3. Expiration et révocation contournables (moyenne)

**Où :** `crates/sign/src/pdf.rs` `finish_validation()` et `check_dss_revocation()`.

- Sans horodatage de confiance, la chaîne et la validité du certificat sont évaluées à
  `signing_time`, une heure **fournie par le signataire** (`/M` ou l'attribut CMS). Avec une clé
  volée sur un certificat expiré, il suffit d'antidater la signature pour obtenir « Valid », sans
  aucun avertissement d'expiration. Sans preuve d'existence (horodatage de confiance), la
  validation ETSI EN 319 102-1 se fait à l'heure courante.
- La révocation n'est vérifiée qu'à partir des CRL/OCSP **embarquées dans le document** (DSS),
  donc choisies par l'attaquant, qui n'y mettra rien. Il n'existe aucune requête OCSP/CRL en
  ligne. Un certificat révoqué après une compromission reste donc « Valid ».

**Correctif :** valider à l'heure courante quand il n'y a pas d'horodatage de confiance (ou au
minimum passer en « Unknown » si le certificat est expiré aujourd'hui). Ajouter une vérification de
révocation en ligne, en opt-in (OCSP puis CRL, avec cache, et désactivable pour respecter la
politique « pas de réseau par défaut »). Quand aucune information de révocation n'est disponible,
l'afficher explicitement dans les détails.

## 4. Aléa non cryptographique pour les clés AES-256 (moyenne)

**Où :** `crates/engine/src/lib.rs` `Ctx::entropy()`, utilisé pour le `seed` de
`NewEncryption` (clé de fichier, sels U/O) et pour le mot de passe propriétaire aléatoire.

`entropy()` hache un compteur avec `RandomState::new().build_hasher()`. La bibliothèque standard
ne garantit pas que `RandomState` soit cryptographique : SipHash-1-3, clé tirée une seule fois par
thread puis incrémentée. Toute l'entropie de la clé de fichier de 256 bits dépend de cette clé
SipHash de 128 bits et d'une construction qui n'a pas été conçue comme DRBG. Il n'y a pas
d'attaque pratique connue, mais c'est la clé de tout le document.

**Correctif :** `getrandom::fill(&mut seed)`. La dépendance est déjà présente dans `ui-egui` et
`sign`, et elle fonctionne en WASM avec la feature `js`. `RandomState` peut rester pour les `/NM`
des annotations.

## 5. Serveur MCP sans confinement par défaut (moyenne)

**Où :** `apps/pdfcraft-cli/src/main.rs`, `crates/automation/src/lib.rs` `resolve()`.

`pdfcraft-cli mcp` sans `--root` donne à l'agent un accès à tout le système de fichiers :
`doc_open` et `sign_trust` en lecture, `doc_save`, `image_save`, `doc_export_*` et
`page_extract` en écriture. `sign_document` signe aussi avec les identités `windows:`, dont les
clés logicielles signent sans aucune invite. Le contenu des PDF traités (texte, commentaires,
métadonnées) arrive directement dans le contexte du LLM : c'est un vecteur d'**injection de
prompt**. Un PDF peut demander à l'agent d'écrire un fichier dans un emplacement de démarrage
automatique ou de signer un document.

Le confinement `--root` lui-même est solide : résolution lexicale de `..`, refus des liens
cassés et des partages UNC, comparaison canonique. Le reste n'est qu'une TOCTOU résiduelle
inhérente.

**Correctif :** faire de `--root` la valeur par défaut (répertoire courant), et exiger un
`--unconfined` explicite pour s'en passer. Exiger un opt-in séparé (`--allow-signing`) pour les
outils `sign_document`, `sign_id_create` et `doc_print`.

## 6. Chaîne d'approvisionnement CI (faible)

`release.yml` épingle `actions/checkout` par SHA dans certaines étapes, mais pas
`dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `actions/upload-artifact@v4` ni
`actions/download-artifact@v4`, qui tournent dans les jobs disposant des secrets Apple et Azure.
Un tag réécrit (cf. l'incident tj-actions/changed-files, 2025) exfiltrerait les certificats de
signature de code. `ci.yml` ne déclare pas de `permissions:` au niveau du workflow.

**Correctif :** épingler toutes les actions par SHA (Dependabot sait les mettre à jour), ajouter
`permissions: contents: read` à `ci.yml`, et désactiver `rust-cache` (`save-if: false`) dans les
jobs de signature pour exclure tout empoisonnement de cache. Les crates de `vendor/` (hayro, lopdf,
winit, egui-wgpu) échappent à `cargo deny advisories` : à suivre manuellement.

## 7. Canal de contrôle UI (faible)

**Où :** `crates/ui-egui/src/control.rs` `serve()`.

Le canal est bien conçu : opt-in, `127.0.0.1`, jeton de 128 bits issu de l'OS, comparaison en temps
constant, fichier en `0o600` écrit par renommage. Deux points restent :
- `BufReader::lines()` n'a pas de limite : avant l'authentification, un processus local peut
  envoyer une ligne de plusieurs Go. De plus, une erreur JSON avant l'authentification ne ferme pas
  la connexion.
- Un thread est créé par connexion, sans plafond.

**Correctif :** limiter la ligne (par exemple `take(1 MiB)`), fermer à la première requête non
authentifiée quelle qu'elle soit, et plafonner les connexions simultanées. Sous Windows, le fichier
de contrôle hérite des ACL du dossier : le documenter, ou poser une ACL propriétaire seule.

## 8. Remarques diverses (info)

- RC4-40 et RC4-128 restent sélectionnables (`protect` : `compatibility`). Afficher un
  avertissement explicite, car RC4-40 se casse par force brute en quelques heures.
- L'IV AES dérivé de `SHA-256(clé ‖ objet ‖ clair)` est déterministe : deux objets identiques à
  la même place produisent le même chiffré. C'est acceptable, mais à documenter.
- Les permissions PDF (impression, copie) ne sont pas une protection quand il n'y a pas de mot de
  passe d'ouverture. L'interface devrait le dire, comme Acrobat.
- `rsa = "=0.10.0-rc.18"` est une pré-version épinglée, et RUSTSEC-2023-0071 (Marvin) est ignoré
  (justifié : vérification seulement). À revoir dès qu'une version corrigée sort.
- La confiance accordée par `sign_trust` via MCP est limitée à la session : c'est correct.

## Points forts constatés

- **Mémoire et robustesse :** aucun `unsafe`, `deny(clippy::unwrap_used, …)` dans les 30 crates,
  `catch_unwind` aux points d'entrée, fuzzing par mutation dans `xtask`, plafonds de
  décompression (`filters`), document en lecture seule s'il dépasse les limites.
- **JavaScript :** boa sans accès fichier, réseau ni minuterie, avec limites de boucles, de
  récursion et de délai. `submitForm`/`mailDoc` ne font rien. Les effets passent par l'hôte.
- **Liens :** seuls `http`, `https` et `mailto` sont acceptés, avec confirmation de l'utilisateur.
  Les caractères invisibles et les `mailto:` avec `attach=`/`insert=` sont refusés (même encodés
  plusieurs fois). Les URL sont validées par le parseur WHATWG pour éviter le repli
  « fichier local » de `webbrowser`. Une seule demande en attente à la fois.
- **Mises à jour :** TLS avec les racines de l'OS, réponse plafonnée à 1 MiB, seules les URL sous
  la page des releases sont proposées, aucun téléchargement ni exécution automatique.
- **Caviardage :** vérification de résidus fail-closed, réécriture complète forcée, écriture des
  seuls objets atteignables (les anciennes révisions disparaissent), nettoyage de l'arbre de
  structure (`Alt`/`ActualText`).
- **Signatures :** construction de chaîne stricte (CA, `keyCertSign`, `pathLen`, validité),
  extensions critiques inconnues signalées, EKU OCSP exigée pour un répondeur délégué, DSS limité
  aux clés standard. Les modifications du catalogue existant sont bien détectées.
- **XML (XFA/XFDF) :** `roxmltree`, sans résolution d'entités externes.

## Priorités recommandées

1. Corriger le n°1 (trailer `/Root` et catalogue neuf) avec des tests de non-régression. C'est la
   seule faille qui permet de falsifier silencieusement un document signé.
2. Supprimer `/Thumb` lors du caviardage (n°2). Le correctif est simple et l'impact sur la
   confidentialité est direct.
3. Utiliser `getrandom` pour les clés (n°4). Une ligne suffit.
4. Revoir le modèle temporel et la révocation des signatures (n°3).
5. Confinement MCP par défaut et épinglage des actions CI (n°5, n°6).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-12 | major | First security review: signature ISA bypass, thumbnails after redaction, signing-time/revocation model, key entropy, MCP default confinement, CI pinning |
