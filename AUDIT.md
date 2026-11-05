# AUDIT vibemk 0.6.4 : sécurité, exactitude, portabilité (contre-audit A4)

**Résultat pytest (A6, réel) :** `PYTHONUTF8=1 .venv-ref/Scripts/python -m pytest -q -p no:cacheprovider --ignore-glob='tests/test_live_*'` : **464 passés, 0 échoué, 11 ignorés** en 30,65 s (aucun test rouge ; les ignorés incluent le test `time.tzset` sous Windows).

**États finaux :** 21 constats corrigés avec test ou fichier cité, 0 « à corriger ». Non corrigés : S2/E9b, E5, E7, E9c, E10b, E10c, P3, P4, P7 (raison dans la colonne État). Réfutés ou positifs : P6, P9, P12.

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
| S2 / E9b | `force_foreign_changes: True` en dur à l'activation automatique | Moyenne | base.py:158 ; appelée via dispatch.py:144 → :160 ; l'outil dédié a False par défaut (configuration.py:56,74) | `activate_changes=true` publie aussi les changements en attente d'un autre utilisateur | Passer False ou un réglage explicite | **Confirmé** | non (avec False on obtient un 409 en présence de changements étrangers ; décision produit) | A5b | non corrigé (changement de comportement à décider) |
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
| E5 | DELETE ou PUT rejoué après 5xx : faux échec (404/412) | Basse | client.py:48, :282-288 | La suppression a eu lieu, le rejeu reçoit 404, le handler annonce un échec | 404 sur un DELETE rejoué = succès | **Confirmé** (DÉDUIT) | non (sémantique d'erreur) | A5b | non corrigé (décision produit) |
| E6 | Branche 412 de `update_host` morte | Basse | hosts.py:715-723 inatteignable : client.py:298 lève sur tout non-2xx ; l'exception tombe dans hosts.py:733-737 | Un second écrivain reçoit « Unexpected error … HTTP 412 » au lieu du conseil de relire | `except CheckMKAPIError` avec status 412/428 renvoyant le message de concurrence | **Confirmé** ; même code mort à hosts.py:623 (`if not success`) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E6_update_host_412_gives_the_concurrency_advice |
| E7 | Pas d'If-Match sur move, rename, aux_tag, user_role | Info | hosts.py:772, 790-793 ; aux_tags.py:93 ; user_roles.py:199 | Hypothétique : 428 si l'endpoint exige l'ETag | Ajouter `_if_match_header` après preuve | **Non prouvé** : aucune exigence trouvée dans la doc, aucun 428 observé | non | – | non corrigé (exigence non prouvée) |
| E8 | Repli silencieux sur ETag `*` | Moyenne | base.py:116-117, :130 ; service_groups.py:189 ; rules.py:390 | Un GET 5xx ou un ETag absent : écrasement concurrent sans signal (aggravé par E1) | `logger.warning` au repli | **Confirmé** | oui pour le warning, non pour « échouer » | A5b | corrigé (warning seulement) — test tests/test_audit_a5b.py::test_E8_wildcard_fallback_is_logged |
| E9a | « ✅ Changes activated » alors que le job est seulement accepté | Basse | base.py:166 (docstring l.140-141) | L'activation échoue ensuite et l'utilisateur croit le contraire | Libellé « Activation started » | **Confirmé** | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E9a_activation_only_says_started |
| E9c | Course 409 « no changes » rapportée comme un échec | Basse | base.py:161-162 | Changements activés par un tiers entre le GET et le POST | Mapper 409 vers une info | **Confirmé** (DÉDUIT) | non (forme du 409 non vérifiée) | A5b | non corrigé (corps du 409 réel inconnu) |
| E10a | `time_ago` sur une date future : « -17999s ago » | Basse | base.py:39 | Horloges désynchronisées | `max(0, diff)` | **Confirmé** (mesuré par A2) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E10a_time_ago_future_never_negative |
| E10b | Downtimes : ISO aware affichées en UTC, nombres en heure locale | Basse | downtimes.py:691-696, 710-714 | Deux fuseaux différents dans une même liste | `.astimezone()` sur la valeur aware | **Partiel** (lu, non rejoué) | oui | A5b | non corrigé — le correctif .astimezone() casse tests/test_handlers_downtimes_times.py::test_an_iso_string_with_z_is_understood (affichage UTC fixé par ce test) ; choix de comportement à trancher, annulé |
| E10c | `fromisoformat` naïf interprété en heure locale | Info | downtimes.py:13-16, 869, 1008, 1018 | Décalage si CheckMK envoie sans offset | Dépend du format réel de l'API | **Partiel** (DÉDUIT, format de l'API inconnu) | non | – | non corrigé (format de l'API non prouvé) |
| E11 | AttributeError non rattrapé dans l'activation automatique | Moyenne | base.py:151, :160 (seul `except CheckMKError` à la l.161) ; dispatch.py:144 hors try, docstring :156-157 « never raises » | pending `data: null` : exception sortie de call_tool, le rapport de l'écriture déjà faite est perdu | Normaliser `(pending.get("data") or {})` et corriger E2 | **Confirmé** (mesuré par A2 ; même cause qu'E2) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_E11_activation_survives_null_pending_data |
| N1 (nouveau) | `update_host` plante si `data` vaut null | Basse | hosts.py:630, :634 (`current_config["data"].get(...)`) ; même motif à :949 | Corps vide ou null : AttributeError rattrapé par `except Exception` (l.733) mais message trompeur | `(current_config.get("data") or {})` | **Confirmé** par lecture (même forme qu'E2, mesurée) | oui | A5b | corrigé — test tests/test_audit_a5b.py::test_N1_update_host_with_null_data_does_not_raise |

### Portabilité, CI, docs

| Id | Titre | Sév. | Preuve | Scénario | Correctif | Verdict contre-audit | Sûr | Lot | État |
|---|---|---|---|---|---|---|---|---|---|
| P1 | 4 tests lisent les sources en cp1252 | Moyenne (au lieu de haute : tests seulement) | test_activate_changes_param.py:84, test_api_paths.py:86, test_handlers_service_params.py:77, test_tool_registry.py:185 | Rouges sous Windows sans PYTHONUTF8 | `read_text(encoding="utf-8")` | **Confirmé** (rejoué : 4 de mes 5 échecs) | oui | A5c | corrigé — tests/test_activate_changes_param.py, test_api_paths.py, test_handlers_service_params.py, test_tool_registry.py verts sans PYTHONUTF8 (A5c : 5 échecs avant) |
| P2 | `time.tzset` sans garde | Moyenne | test_handlers_metrics.py:30, :42 | Échec et erreur au teardown sous Windows | `skipif(not hasattr(time,"tzset"))` sur le test et dans la fixture | **Confirmé** (rejoué : 1 échec et 1 erreur) | oui | A5c | corrigé — tests/test_handlers_metrics.py (skipif tzset ; ignoré sous Windows) |
| P3 | CI Linux seulement | Moyenne | ci.yml:27,56,94 ; release.yml:30,85,108 | P1 et P2 sont invisibles en CI | Matrice OS | **Confirmé** | non (les noms des checks requis changent) | A5c | non corrigé (protection de branche à coordonner) |
| P4 | Build de test_packaging non isolé | Basse | test_packaging.py:122-131 | Deux pytest concurrents se cassent mutuellement | Construire depuis une copie | **Partiel** (un seul reproduit) | non | A5c | non corrigé (cause non prouvée) |
| P5 | `read_text()` latent dans test_workflow_pinning | Basse | test_workflow_pinning.py:23 | Un workflow accentué casserait le test | `encoding="utf-8"` | **Confirmé** | oui | A5c | corrigé — tests/test_workflow_pinning.py (encoding=utf-8), vert |
| P6 | Actions épinglées par SHA, permissions minimales | Info | ci.yml:16-17 ; release.yml:19-20 | – | – | **Réfuté comme défaut** (constat positif) | – | – | réfuté |
| P7 | Chemins POSIX en dur (Makefile, CI) | Basse | Makefile:92-95 ; ci.yml:114-123 | `make package` hors POSIX | Documenter | **Confirmé** | non (choix de périmètre) | A5c | non corrigé (choix de périmètre) |
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

Aucun test rouge dans la suite lancée en A6. Restent des décisions ou preuves, pas des défauts de test :
- S2/E9b : décider de `force_foreign_changes` (True en dur) à l'activation automatique.
- E5 : sémantique d'un DELETE rejoué qui reçoit 404.
- E9c : forme réelle du 409 « no changes » à vérifier.
- E10b : trancher l'affichage UTC ou local des downtimes avant `.astimezone()` (le test `test_an_iso_string_with_z_is_understood` fixe UTC).
- E7, E10c : non prouvés, à rouvrir si un 428 ou un format d'API sans offset est observé.
- P3 : matrice d'OS en CI (protection de branche à coordonner). P4 : cause non prouvée. P7 : choix de périmètre POSIX.
- S1 : Authorization n'est plus envoyé sur aucune redirection, y compris vers le même hôte (plus strict que le filtre scheme+netloc proposé).
- S4 : le double encodage `%252f` n'est pas traité (un seul unquote, comme le serveur).
- hosts.py:733 : un `CheckMKAPIError` hors 412/428 est relancé et rattrapé par `handle()` (message d'erreur différent, non couvert par un test).
- E10b / flaky : un échec isolé de `test_handlers_downtimes_times::test_an_iso_string_with_z_is_understood` a été vu une fois par A5c, non reproduit (464 passés ici).
