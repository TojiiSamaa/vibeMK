# AUDIT vibemk 0.6.4 : sécurité, exactitude, portabilité (contre-audit A4)

**Résultat pytest (A6, réel) :** `PYTHONUTF8=1 .venv-ref/Scripts/python -m pytest -q -p no:cacheprovider --ignore-glob='tests/test_live_*'` : **464 passés, 0 échoué, 11 ignorés** en 30,65 s (aucun test rouge ; les ignorés incluent le test `time.tzset` sous Windows).

**États finaux :** 21 constats corrigés avec test ou fichier cité, 0 « à corriger ». Le 07/11/2026, S2/E9b, E5, E9c, E10b, P3 et P4 ont été corrigés (test et mutation), E7 et E10c réfutés par mesure sur CheckMK réel, P7 documenté : plus aucun constat ouvert. Réfutés ou positifs : P6, P9, P12.

Sources : audits A1 (sécurité, S1-S5), A2 (exactitude, E1-E11), A3 (portabilité/CI, P1-P12), relus ligne par ligne par un contradicteur.
Reproductions du contre-audit : `scratchpad/mut/wfx3y8zj-A4/a4.py`, lancé avec `PYTHONUTF8=1 .venv-ref/Scripts/python`. Sortie :
`S3 TypeError: comparing strings with non-ASCII characters is not supported` ;
`E1 dict keys: ['etag'] get ETag -> None | HTTPMessage.get -> "abc"` ;
`E2 AttributeError` pour chacun des trois cas (data=None, extensions=None, data=[]).
Tests rejoués sans PYTHONUTF8 (Windows) : `pytest tests/test_activate_changes_param.py tests/test_api_paths.py tests/test_handlers_service_params.py tests/test_tool_registry.py tests/test_handlers_metrics.py tests/test_etag_locking.py tests/test_client_retry.py`, résultat : **5 failed, 87 passed, 1 error**. Les 5 échecs et l'erreur sont ceux de P1 et P2.

## Résumé

Le tableau compte 32 lignes : S1-S5, E1-E11 (E9 découpé en a/b/c, E10 en a/b/c, E9b fusionné avec S2), P1-P12, plus un constat nouveau, N1.

| Sévérité (recalibrée) | Nombre | Ids |
|---|---|---|
| Haute | 1 | S1 |
| Moyenne | 9 | S2 (=E9b), E1, E3, E4, E8, E11, P1, P2, P3 |
| Basse | 17 | S3, S4, S5, E2, E5, E6, E9a, E9c, E10a, E10b, N1, P4, P5, P7, P8, P10, P11 |
| Info (positif ou non prouvé) | 5 | E7, E10c, P6, P9, P12 |

Verdicts : 24 confirmés, 6 partiels (E1, S4, P4, P11, E10b, E10c), 1 réfuté comme défaut (P6, qui est un constat positif), 1 non prouvé (E7).

## Constats

Lots : A5a = `src/vibemk/server/**`, `handlers/connection.py`, `api/paths.py`. A5b = `api/client.py`, `api/exceptions.py`, `config/**`, `handlers/*` hors connection.py. A5c = `tests/**`, `.github/**`, `pyproject.toml`, README et docs.

### Sécurité

| Id | Titre | Sév. | Preuve | Scénario | Correctif | Verdict contre-audit | Sûr | Lot | État |
|---|---|---|---|---|---|---|---|---|---|
| S1 | Authorization suit une redirection vers un autre hôte | Haute | client.py:204 (`Request(url, headers=request_headers)`), :214 (`urlopen` avec le handler de redirection par défaut), :162 (défaut `follow_redirects=True`) | Un 302 venant d'un proxy, d'un SSO ou d'un MITM en http envoie `Authorization: Basic` à un hôte tiers. Mesuré par A1 (proof.py). | Sous-classe de `HTTPRedirectHandler` qui n'accepte que le même scheme+netloc et refuse (ou retire Authorization) sinon, utilisée comme opener par défaut | **Confirmé** : les en-têtes passés au constructeur de `Request` sont dans `req.headers`, que urllib recopie sur un 3xx. Aucune protection plus haut. | oui (filtre même hôte ; pas le défaut sans redirection) | A5b | corrigé — test tests/test_audit_a5b.py::test_S1_authorization_not_sent_to_another_host (Authorization retiré sur toute redirection, même hôte compris) |
| S2 / E9b | `force_foreign_changes: True` en dur à l'activation automatique | Moyenne | base.py:158 ; appelée via dispatch.py:144 → :160 ; l'outil dédié a False par défaut (configuration.py:56,74) | `activate_changes=true` publie aussi les changements en attente d'un autre utilisateur | Passer False ou un réglage explicite | **Confirmé** | non (avec False on obtient un 409 en présence de changements étrangers ; décision produit) | A5b | **corrigé le 07/11/2026** (Python et Rust) : l'activation automatique envoie `force_foreign_changes: false` ; le refus mesuré (401 sur 2.3, 403 sur 2.5, « There are changes from other users… ») est rapporté comme tel, avec la marche à suivre (`vibemk_activate_changes` avec `force_foreign_changes=true`). Tests : `tests/test_handlers_configuration.py`, `rust/tests/configuration.rs`, mutation tuée ; vérifié en réel (`scripts/live_foreign_changes.py`, Python et Rust sur 2.3 et 2.5) |
| S3 | TypeError (500) sur un jeton Bearer non ASCII | Basse | http_auth.py:75 ; appelant :58 sans try | `Authorization: Bearer é` donne un 500 au lieu d'un 401 ; pas de contournement | `compare_digest(presented.strip().encode(), self._token.encode())` | **Confirmé** (a4.py : TypeError) | oui | A5a | corrigé — test tests/test_audit_a5a.py::test_s3_non_ascii_bearer_is_401_not_500 |
| S4 | Antislash non filtré par `_refuse_foreign_url` (et `%252f`, double encodage) | Basse | connection.py:157-158 (`unquote(...).split("/")`) | `.../x\..\..\y` est accepté ; reste sur le même hôte, en GET | Refuser `\` après unquote | **Partiel** : vrai, mais non exploitable sur Apache/Linux ; même hôte imposé à la l.151 | oui | A5a | corrigé — test tests/test_audit_a5a.py::test_s4_backslash_in_path_is_refused (antislash brut, %5c, %5C ; double encodage %252f non traité) |
| S5 | TLS désactivable et http accepté sans avertissement | Basse | client.py:94-95 ; settings.py:35 (défaut sûr) | `verify_ssl=false` : les identifiants Basic exposés au MITM sans signal | `logger.warning` au démarrage si `not verify_ssl` ou scheme http | **Confirmé** (défauts sûrs, seul le signal manque) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_S5_warns_when_tls_verification_is_off |

### Exactitude

| Id | Titre | Sév. | Preuve | Scénario | Correctif | Verdict contre-audit | Sûr | Lot | État |
|---|---|---|---|---|---|---|---|---|---|
| E1 | ETag lu avec la casse exacte | Moyenne (au lieu de haute) | client.py:260 `dict(headers)` ; base.py:127 ; hosts.py:627 ; service_groups.py:185-186 | Un proxy qui renvoie `etag` en minuscules : base.py rend `*` (verrou optimiste coupé) ; hosts.py se rabat sur meta_data (l.630) puis n'envoie aucun If-Match | Dans `_result`, garder un accès insensible à la casse (par exemple `headers.get` sur le HTTPMessage pour ETag, ou des clés normalisées) | **Partiel** : la cause est prouvée (a4.py : le HTTPMessage est insensible à la casse, le `dict()` le rend sensible). Mais urllib parle HTTP/1.1 et CheckMK émet `ETag` : le déclencheur exige un proxy qui réécrit la casse. Le 428 est DÉDUIT. | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E1_etag_header_found_whatever_its_case |
| E2 | `_extract_etag` plante si data ou extensions valent null ou une liste | Basse | base.py:129 | GET au corps inattendu : AttributeError hors CheckMKError | `(x or {})` et `isinstance(dict)` à chaque niveau | **Confirmé** (a4.py) ; peu probable avec un vrai CheckMK | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E2_extract_etag_survives_odd_bodies (4 cas) |
| E3 | Le rejeu après 5xx perd `follow_redirects` | Moyenne | client.py:288 ; `_handle_http_error` (l.263-273) ne reçoit pas le paramètre ; appelants discovery.py:441,444 (GET, donc rejouables) | Découverte : 503 puis rejeu qui suit les 302 et finit en « infinite loop » | Passer `follow_redirects` à `_handle_http_error` puis au rejeu | **Confirmé** (discovery.py:127 est en POST, donc non rejoué ; 441 et 444 sont en GET) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E3_replay_after_5xx_keeps_follow_redirects_false |
| E4 | Retry-After sans plafond | Moyenne | client.py:316, :287 | `Retry-After: 3600` : l'appel MCP dort une heure | `min(delay, 30.0)` | **Confirmé** | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E4_retry_after_is_capped |
| E5 | DELETE ou PUT rejoué après 5xx : faux échec (404/412) | Basse | client.py:48, :282-288 | La suppression a eu lieu, le rejeu reçoit 404, le handler annonce un échec | 404 sur un DELETE rejoué = succès | **Confirmé** (DÉDUIT) | non (sémantique d'erreur) | A5b | **corrigé le 07/11/2026** : un 404 reçu par un DELETE *rejoué* (après 5xx/429) est un succès ; un 404 au premier essai et un GET rejoué restent des erreurs. Tests : `tests/test_client_retry.py`, `rust/tests/client.rs`, deux mutations tuées. Le PUT rejoué (412) n'est pas traité : aucun PUT n'est rejoué sans ETag frais |
| E6 | Branche 412 de `update_host` morte | Basse | hosts.py:715-723 inatteignable : client.py:298 lève sur tout non-2xx ; l'exception tombe dans hosts.py:733-737 | Un second écrivain reçoit « Unexpected error … HTTP 412 » au lieu du conseil de relire | `except CheckMKAPIError` avec status 412/428 renvoyant le message de concurrence | **Confirmé** ; même code mort à hosts.py:623 (`if not success`) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E6_update_host_412_gives_the_concurrency_advice |
| E7 | Pas d'If-Match sur move, rename, aux_tag, user_role | Info | hosts.py:772, 790-793 ; aux_tags.py:93 ; user_roles.py:199 | Hypothétique : 428 si l'endpoint exige l'ETag | Ajouter `_if_match_header` après preuve | **Non prouvé** : aucune exigence trouvée dans la doc, aucun 428 observé | non | – | **réfuté par mesure** : la sweep appelle move_host, move_folder, rename_host, update_aux_tag et update_user_role sur 2.3.0p52 et 2.5.0p18 (270/270, Python et Rust), aucun 428. move_host et move_folder envoient déjà If-Match |
| E8 | Repli silencieux sur ETag `*` | Moyenne | base.py:116-117, :130 ; service_groups.py:189 ; rules.py:390 | Un GET 5xx ou un ETag absent : écrasement concurrent sans signal (aggravé par E1) | `logger.warning` au repli | **Confirmé** | oui pour le warning, non pour « échouer » | A5b | corrigé (warning seulement) — test tests/test_audit_a5b.py::test_E8_wildcard_fallback_is_logged |
| E9a | « ✅ Changes activated » alors que le job est seulement accepté | Basse | base.py:166 (docstring l.140-141) | L'activation échoue ensuite et l'utilisateur croit le contraire | Libellé « Activation started » | **Confirmé** | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E9a_activation_only_says_started |
| E9c | Course 409 « no changes » rapportée comme un échec | Basse | base.py:161-162 | Changements activés par un tiers entre le GET et le POST | Mapper 409 vers une info | **Confirmé** (DÉDUIT) | non (forme du 409 non vérifiée) | A5b | **corrigé le 07/11/2026** : corps réel mesuré sur 2.3 et 2.5, **422** « Currently there are no changes to activate. » (pas 409) ; rapporté comme « No pending changes » dans les deux chemins d'activation. Tests Python et Rust, mutation tuée |
| E10a | `time_ago` sur une date future : « -17999s ago » | Basse | base.py:39 | Horloges désynchronisées | `max(0, diff)` | **Confirmé** (mesuré par A2) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E10a_time_ago_future_never_negative |
| E10b | Downtimes : ISO aware affichées en UTC, nombres en heure locale | Basse | downtimes.py:691-696, 710-714 | Deux fuseaux différents dans une même liste | `.astimezone()` sur la valeur aware | **Partiel** (lu, non rejoué) | oui | A5b | **corrigé le 07/11/2026** : mesuré, CheckMK 2.3 et 2.5 renvoient les heures de downtime en ISO UTC (`+00:00`) alors que l'heure saisie est lue en heure locale ; l'affichage est maintenant en heure locale (Python `.astimezone()`, Rust `with_timezone`). Le test `test_an_iso_string_with_z_is_understood` ne passait que sur une machine en UTC (cause probable de l'échec isolé vu par A5c) ; il tourne désormais en +5:30 fixe. Mutation tuée |
| E10c | `fromisoformat` naïf interprété en heure locale | Info | downtimes.py:13-16, 869, 1008, 1018 | Décalage si CheckMK envoie sans offset | Dépend du format réel de l'API | **Partiel** (DÉDUIT, format de l'API inconnu) | non | – | **réfuté par mesure** : CheckMK 2.3 et 2.5 envoient toujours un offset (`2030-01-01T08:00:00+00:00` pour un downtime posé à 10:00+02:00) ; la branche naïve n'est pas atteinte |
| E11 | AttributeError non rattrapé dans l'activation automatique | Moyenne | base.py:151, :160 (seul `except CheckMKError` à la l.161) ; dispatch.py:144 hors try, docstring :156-157 « never raises » | pending `data: null` : exception sortie de call_tool, le rapport de l'écriture déjà faite est perdu | Normaliser `(pending.get("data") or {})` et corriger E2 | **Confirmé** (mesuré par A2 ; même cause qu'E2) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E11_activation_survives_null_pending_data |
| N1 (nouveau) | `update_host` plante si `data` vaut null | Basse | hosts.py:630, :634 (`current_config["data"].get(...)`) ; même motif à :949 | Corps vide ou null : AttributeError rattrapé par `except Exception` (l.733) mais message trompeur | `(current_config.get("data") or {})` | **Confirmé** par lecture (même forme qu'E2, mesurée) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_N1_update_host_with_null_data_does_not_raise |

### Portabilité, CI, docs

| Id | Titre | Sév. | Preuve | Scénario | Correctif | Verdict contre-audit | Sûr | Lot | État |
|---|---|---|---|---|---|---|---|---|---|
| P1 | 4 tests lisent les sources en cp1252 | Moyenne (au lieu de haute : tests seulement) | test_activate_changes_param.py:84, test_api_paths.py:86, test_handlers_service_params.py:77, test_tool_registry.py:185 | Rouges sous Windows sans PYTHONUTF8 | `read_text(encoding="utf-8")` | **Confirmé** (rejoué : 4 de mes 5 échecs) | oui | A5c | corrigé — tests/test_activate_changes_param.py, test_api_paths.py, test_handlers_service_params.py, test_tool_registry.py verts sans PYTHONUTF8 (A5c : 5 échecs avant) |
| P2 | `time.tzset` sans garde | Moyenne | test_handlers_metrics.py:30, :42 | Échec et erreur au teardown sous Windows | `skipif(not hasattr(time,"tzset"))` sur le test et dans la fixture | **Confirmé** (rejoué : 1 échec et 1 erreur) | oui | A5c | corrigé — tests/test_handlers_metrics.py (skipif tzset ; ignoré sous Windows) |
| P3 | CI Linux seulement | Moyenne | ci.yml:27,56,94 ; release.yml:30,85,108 | P1 et P2 sont invisibles en CI | Matrice OS | **Confirmé** | non (les noms des checks requis changent) | A5c | **corrigé le 07/11/2026** : job `test-os` (windows-latest, macos-latest) ajouté à `ci.yml`, séparé pour ne pas renommer les checks requis « test (3.x) ». Non encore vu tourner sur GitHub |
| P4 | Build de test_packaging non isolé | Basse | test_packaging.py:122-131 | Deux pytest concurrents se cassent mutuellement | Construire depuis une copie | **Partiel** (un seul reproduit) | non | A5c | **corrigé le 07/11/2026** : le build part d'une copie du projet dans un dossier temporaire ; deux pytest lancés en même temps passent (15/15 chacun) et rien n'est écrit dans le checkout |
| P5 | `read_text()` latent dans test_workflow_pinning | Basse | test_workflow_pinning.py:23 | Un workflow accentué casserait le test | `encoding="utf-8"` | **Confirmé** | oui | A5c | corrigé — tests/test_workflow_pinning.py (encoding=utf-8), vert |
| P6 | Actions épinglées par SHA, permissions minimales | Info | ci.yml:16-17 ; release.yml:19-20 | – | – | **Réfuté comme défaut** (constat positif) | – | – | réfuté |
| P7 | Chemins POSIX en dur (Makefile, CI) | Basse | Makefile:92-95 ; ci.yml:114-123 | `make package` hors POSIX | Documenter | **Confirmé** | non (choix de périmètre) | A5c | **documenté** : `make package` est POSIX, le Makefile le dit et renvoie aux étapes du job CI `package` sous Windows |
| P8 | Groupe dependabot incomplet | Basse | dependabot.yml:6-25 | PR séparées pour build, twine, types-* | Ajouter les motifs | **Confirmé** | oui | A5c | corrigé — .github/dependabot.yml (sans test dédié) |
| P9 | Packaging cohérent | Info | version.py:11, server.json:10,15 | – | – | **Confirmé** (positif) | – | – | réfuté (rien à corriger) |
| P10 | INSTALL.md épingle 0.6.1 | Basse | INSTALL.md:286 | L'exemple installe une version périmée | 0.6.4 ou pas de numéro | **Confirmé** | oui | A5c | corrigé — INSTALL.md:286 passé à 0.6.4 (sans test dédié) |
| P11 | Variables d'environnement mal documentées | Basse | settings.py:98-145 ; LOGFILE lue à utils/logging.py:32 et cli.py:94 | README et .env.example partiels ; LOGFILE absente aussi d'INSTALL.md | Compléter .env.example et INSTALL.md (16 variables) | **Partiel** : 16 variables et non 15, INSTALL.md ne les couvre pas toutes | oui | A5c | corrigé — INSTALL.md §5.4 et .env.example (16 variables, sans test dédié) |
| P12 | Aucun nombre d'outils périmé dans les docs | Info | get_all_tools() = 154 | – | – | **Confirmé** (positif) | – | – | réfuté (rien à corriger) |

Notes du contradicteur :
- Thème « injection de chemin » d'A1 : l'énumération des handlers est fausse (`path_segment` apparaît dans de nombreux handlers, et debug.py:311 utilise `quote(safe='')`). La conclusion tient quand même, avec `check_endpoint` (client.py:173) en second filet. Rien d'exploitable.
- La remarque d'A1 sur client.py:288 est le même défaut qu'E3, traité une seule fois.

## À reporter dans le port Rust

Comportements portés (client, dispatch, http_auth, handlers) dont le constat est confirmé. Le port ne doit pas en recopier le bug :
1. **S1 (client)** : ne jamais renvoyer Authorization sur une redirection vers un autre scheme ou hôte. reqwest le fait par défaut (il retire les en-têtes sensibles entre hôtes), mais il faut une politique de redirection explicite et un test.
2. **E3 (client)** : un rejeu garde toutes les options de la requête d'origine, `follow_redirects` compris.
3. **E4 (client)** : plafonner Retry-After (30 s).
4. **E1 (client)** : en-têtes de réponse insensibles à la casse (`HeaderMap`), ne pas les convertir en map sensible à la casse.
5. **E2, E11, N1 (handlers, activation)** : lecture de `data` et `extensions` tolérante à null ou non-objet ; l'activation automatique ne peut jamais faire perdre le rapport d'écriture (dispatch).
6. **E8 (handlers)** : avertir lors d'un repli sur `If-Match: *`.
7. **E6 (handlers)** : traduire 412/428 en message de concurrence (l'erreur arrive comme une erreur HTTP, pas comme `success=false`).
8. **E9a (handlers)** : libellé « activation lancée », pas « activée ».
9. **S2 (handlers/dispatch)** : reproduire le comportement choisi pour `force_foreign_changes` (décision en attente : ne pas figer True sans le décider).
10. **S3 (http_auth)** : comparaison à temps constant sur des octets ; un jeton non ASCII donne un 401, jamais un 500.
11. **S4 (connection)** : rejeter l'antislash et le double encodage dans le filtre d'URL directe.
12. **E10a (handlers)** : `time_ago` borné à 0 pour une date future.
13. **E5 (client)** : à trancher avant le port (DELETE rejoué qui reçoit 404).

## Reste à faire

Plus aucun constat ouvert (07/11/2026). Choix assumés, à rouvrir seulement sur un cas réel :
- S1 : Authorization n'est envoyé sur aucune redirection, même vers le même hôte (plus strict que le filtre scheme+netloc proposé).
- S4 : le double encodage `%252f` n'est pas traité (un seul unquote, comme le serveur).
- E5 : seul le DELETE rejoué est traité ; un PUT rejoué qui reçoit 412 reste une erreur.
- P3 : le job `test-os` n'a pas encore tourné sur GitHub (pas de push possible depuis cette machine sans authentification).

## Test réel : les 154 outils contre CheckMK 2.3 et 2.5 (06-07/11/2026)

Étiquettes : **MESURÉ** = lu dans un fichier JSON ou une sortie de ce run ; **DÉDUIT** = conclusion tirée d'une mesure.

Banc : `scripts/live_sweep.py <label> <commande du serveur>`. Il appelle chacun des 154 outils sur un vrai site (Docker : 2.3.0p52 Raw sur 5080, 2.5.0p18 Community sur 5081), relit 69 des écritures, puis supprime ce qu'il a créé et vérifie qu'il ne reste ni objet de test ni règle qui nomme un hôte de test. Résultats bruts : `scripts/live_sweep_{py,rs}{23,25}.json`.

### Sweeps (MESURÉ, champs `passed`, `steps`, `called`, `listed`, `verified` des JSON, 07/11 00:23-00:31)

| | 2.3.0p52 | 2.5.0p18 |
|---|---|---|
| Python | 270/270 | 270/270 |
| Rust | 270/270 | 270/270 |

Dans les quatre JSON : 154 outils listés, 154 appelés, 0 non appelé, 69 étapes vérifiées. Les quatre ont tourné avec le code final (activation corrigée sur les deux chemins, banc corrigé), un seul sweep à la fois par instance, sur des sites vérifiés propres avant.

Le passage à 270 étapes (257 avant) vient du nettoyage ajouté : 10 `delete_active_check`, 2 `delete_rule` (règles interface et mémoire) et la vérification finale « no rule names a test host any more ».

### Tests live (MESURÉ, sorties pytest et cargo du 07/11, code final)

Variables : `LIVE_SMOKE_TEST`, `LIVE_WRITE_TEST`, `INTEGRATION_TESTS` à true, hôte `vmk-live` créé pour le run puis supprimé (pending 0 après).

| Fichier | 2.3 (5080) | 2.5 (5081) |
|---|---|---|
| `tests/test_live_smoke.py` | 58 passés, 0 échec, 1 ignoré | 58 passés, 0 échec, 1 ignoré |
| `tests/test_live_writes.py` | 18 passés, 0 échec, 1 ignoré | 18 passés, 0 échec, 1 ignoré |
| `tests/test_live_findings.py` | 10 passés | 10 passés |
| `tests/test_integration.py` | 9 passés | 9 passés |
| `rust/tests/smoke.rs` | 1 passé | 1 passé |
| `rust/tests/integration.rs` | 9 passés, 0 échec | 9 passés, 0 échec |

Ignorés : smoke « nothing is failing on this instance right now » (aucun service en échec à détecter) ; writes « needs two open vibemk-probe events » (pas d'événements de la console sur l'hôte).
Le smoke demande un hôte avec agent (service `Check_MK`) : avec `vmk-live` en `no-agent`, `get_service_status` échoue sur les deux versions (MESURÉ, 1 échec chacun), ce qui vient de l'hôte de test et non de vibeMK.

Avant les correctifs de ce run, sur 2.3 : writes 11 passés / 7 échecs (6 créations de check actif refusées en 400 car vibeMK envoyait les formes de 2.5, plus la bakery), et `integration.rs` 8/1 sur les deux versions.

### Changements en attente après l'activation (2.3)

Cause (MESURÉ, rejeu sur 5080) : sur 2.3, un `activate-changes` envoyé pendant qu'une autre activation tourne reçoit HTTP 200 et un nouvel id, mais le site le refuse. Le run se termine en 0,4 s, titre « Activation status: Complete. », avec l'état par site `warning / Locked` (« cmkadmin is currently activating changes on this site »), visible seulement dans le fichier d'état `site_cmk.mk` du conteneur. Dans le sweep, l'activation en cours vient de `set_process_thresholds` avec `run_discovery` (~6 s), l'appel explicite arrivant ~3,9 s après. Les 2 changements créés après le début du run restaient donc en attente, ce qui était le symptôme de py23.

Correctif, sur les deux chemins d'activation : avant de lire les changements en attente, vibeMK attend qu'aucun run de `collections/running` n'ait `is_running == true` (60 s au plus ; une erreur de lecture ne bloque pas).
- `vibemk_activate_changes` : Python `ConfigurationHandler._wait_for_running_activation`, Rust `Base::wait_for_running_activation`.
- Activation lancée par un outil d'écriture (`activate_changes=true`, `set_process_thresholds`) : Python `BaseHandler._run_activation`, Rust `Base::run_activation`.

Tests : `tests/test_handlers_configuration.py` (4) et `rust/tests/configuration.rs` (`activation_waits_for_the_run_already_in_progress`, `write_tools_activation_also_waits_for_the_run_in_progress`), sur les corps réels de 2.3.0p52. Mutation (MESURÉ) : sans l'attente dans `_run_activation` / `run_activation`, le test du chemin d'écriture est rouge (1 échec en Python, 1 sur 14 en Rust), vert avec.

### Défauts du banc trouvés et corrigés (MESURÉ)

- `$PROC_RULE` prenait la première règle de `inventory_processes_rules`, pas celle du sweep. Sur 2.3 le ruleset est vide, c'était donc la bonne. Sur 2.5, qui livre des règles de processus par défaut, chaque sweep supprimait la règle n° 0 de CheckMK et laissait la sienne : le journal d'audit de 5081 compte 5 suppressions « Deleted rule #0 » dans ce ruleset. Il reste 24 règles par défaut sur 5081 ; **le site de test 5081 a perdu 5 règles de processus par défaut** (sans effet sur vibeMK, à recréer si le site sert à autre chose). Le résolveur prend maintenant la règle qui nomme l'hôte du sweep.
- Le sweep laissait 12 règles sur `vmk-h1` (10 checks actifs, interface, mémoire) en annonçant « every test object is gone » : supprimer un hôte ne supprime pas les règles qui le nomment. Elles sont maintenant supprimées par les outils de vibeMK, et la dernière étape le vérifie.
- `rust/tests/integration.rs::service_discovery_workflow` appelait `vibemk_get_all_services`, un outil qui n'existe pas (le bon nom est `vibemk_get_checkmk_services`) : la réponse n'avait pas de `content`. Le test vérifie aussi `isError` maintenant. 9/9 sur les deux versions.
- `test_the_agent_bakery_bakes_or_says_why_it_cannot` reconnaissait l'édition gratuite par `.community` (2.5) seulement ; 2.3 répond `2.3.0p52.cre` (MESURÉ sur `/version`). Corrigé : sur 2.3 le test vérifie que l'outil explique qu'il n'y a pas de bakery.

### Échec possible sur un site neuf

`search_metrics` (`vmk-h1`/PING/rta), HTTP 400 « The requested graph does not exist » : côté CheckMK. Preuve : le même corps envoyé directement à `metric/actions/get/invoke`, sans vibeMK, donne 200 quand les RRD de l'hôte existent et 400 sur 40 appels en 162 s quand ils sont mis de côté ; PING était CRITICAL sans que `PING_*.rrd` apparaisse. Le premier sweep après la création du site tombe dessus, les suivants non (DÉDUIT de py25 le 06/11, 256/257, puis 270/270 le 07/11).

### Downtimes

MESURÉ sur 5081 : `modify_downtime` renvoie l'erreur « Downtime not modified » (7,7 s) et l'heure de fin ne bouge pas. Le cœur Nagios des éditions Raw et Community n'a pas de commande `MODIFY_*_DOWNTIME`, alors que l'API répond 204. Ce n'est pas un défaut de vibeMK.

### Hors live (MESURÉ, 07/11)

`pytest -q` : 504 passés, 87 ignorés, 0 échec. Les 87 ignorés sont tous des tests live désactivés sans leurs variables (59 smoke, 19 writes, 9 integration), lancés ci-dessus sur les deux sites. `cargo test --offline` : 566 passés, 0 échec. black, isort, ruff, mypy, `cargo fmt --check` et clippy `-D warnings` : verts.

### Cas signalé par Tom (rappel du run précédent, non rejoué dans ce run)

`update_rule` sur une règle filtrée par label, et `get_ruleset` : la valeur est modifiée, les `host_label_groups` sont conservés, et `get_ruleset` affiche « Labels: 1 conditions ».

Écarts Python/Rust connus (non couverts par un test) : Rust reconnaît un littéral Python dans `value_raw` avec son propre analyseur. Comparé à `ast.literal_eval` sur environ 90 cas, il ne montre aucun écart, mais trois raccourcis sont commentés dans `rust/src/handlers/rules.rs`.
