<p align="center">
  <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <a href="README.md">English</a> | <a href="README.hi.md">हिन्दी</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (BR)</a>
</p>

<p align="center">
  <img src="https://raw.githubusercontent.com/mcp-tool-shop-org/brand/main/logos/offrig/readme.png" alt="offrig" width="400">
</p>

<p align="center">
  <a href="https://github.com/mcp-tool-shop-org/offrig/actions/workflows/ci.yml"><img src="https://github.com/mcp-tool-shop-org/offrig/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://codecov.io/gh/mcp-tool-shop-org/offrig"><img src="https://codecov.io/gh/mcp-tool-shop-org/offrig/graph/badge.svg" alt="Coverage"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://mcp-tool-shop-org.github.io/offrig/"><img src="https://img.shields.io/badge/Landing_Page-live-blue" alt="Landing Page"></a>
</p>

[![Score M8ven](https://m8ven.ai/badge/mcp/mcp-tool-shop-org-offrig-790exm?v=3bd60e98b65d7124b74c4126c5ce5ce7)](https://m8ven.ai/mcp/mcp-tool-shop-org-offrig-790exm?s=readme)

Exécutez des modèles volumineux sur des GPU RunPod loués, avec la garantie qu’ils ne seront jamais exécutés sur votre propre GPU. Une application de bureau, une interface en ligne de commande et un module complémentaire pour les agents, le tout basé sur une seule bibliothèque Rust.

Le side-car permet à un agent de planifier une session payante dans le cadre d’un budget défini par un humain, de louer les GPU et de transmettre une file d’attente de tâches à un exécuteur distinct. L’exécuteur maintient chaque emplacement de modèle actif, ne révise que les vérifications qui ont échoué et arrête le pod lorsque la file d’attente est vide. Un programme de surveillance termine le pod à la date limite du plan, même si tout le reste est terminé.

## Statut

Testé en direct les 2026-10-02 et 2026-10-03, pour un coût total d’environ 5 $:

- **Frontier :** 4 × RTX PRO 6000 (384 Go) exécutant Qwen3-Coder-480B (AWQ 4 bits) sur SGLang, prêt en 22 minutes, puis 31 transferts effectués en 20 secondes, pour 3,59 $.
- **Données Swarm :** un modèle chargé sert des centaines d’agents simultanément. Le modèle de 480 milliards de paramètres a atteint 4 059 jetons/s avec 512 agents ; un modèle de 30 milliards de paramètres sur une seule carte a atteint 10 147 jetons/s avec 256 agents.
- **Exécuteur :** une file d’attente avec des dépendances et des renvois de révision, fonctionne sans intervention humaine, le pod s’arrête lorsque la file d’attente est vide.
- **Garantie :** le GPU local est resté inactif pendant toutes les exécutions.

Utilisé quotidiennement depuis le 2026-10-07 par deux projets simultanément, chacun dans son propre contexte : exécution de tâches d’entraînement pour aspire-si sur `job` pods, et rendu de séquences musicales pour ai-jam-sessions sur `jam` pods.

Le 2026-10-11, un pod SGLang a enregistré `Application startup complete`, puis `/health` est resté en erreur 503 pendant 7 minutes. Les GPU fonctionnaient à près de 100 % avec une consommation d’environ 100 W, et `/v1/models` a renvoyé 200. Les pods actifs cette semaine utilisaient CUDA 13.0. Ce pod a été placé sur CUDA 13.2. Il n’a pas été démontré que 13.2 en est la cause ; l’épingle permet à un profil de rester sur la version qui fonctionnait. Cette version signale ce problème, et elle ne considère pas un modèle comme prêt tant qu’une complétion ne renvoie pas un choix. Un profil peut également fixer la version CUDA de l’hôte.

Non prouvé sur un pod loué pour cette version : la vérification de préparation d’un seul jeton, les rapports `engine_unresponsive` et `engine_hung`, et une création qui envoie `gpu.allowedCudaVersions`. Ces chemins sont couverts par des tests effectués sur une session SSH simulée et un RunPod simulé. Aucun pod n’a été démarré pour les revérifier. L’énumération v1 `POST /pods` documentée par RunPod, lue le 2026-10-11, s’arrête toujours à 13.0. 13.1 et 13.2 figurent dans la liste d’offrig, car le pod d’écriture a été placé sur 13.2. Un minimum est toujours envoyé, qui n’est que `gpu.minCudaVersion`.

Construit et testé, en attente d’une décision : préparation des poids de Frontier sur un volume réseau, environ 21 $/mois (voir [Préparation](#pondération-en-attente-sur-un-volume-réseau)).

Prochainement : la première véritable file d’attente, planifiée dans son intégralité avant le lancement ; les transferts de code sont compilés et testés sur le pod.

## Ce que cela fait

À partir d’une seule fenêtre (ou d’une seule commande), offrig :

1. affiche votre solde RunPod, les prix actuels des GPU et la durée pendant laquelle le solde durera ;
2. lance un pod pour une catégorie, d’une petite carte sur Ollama à 4 × RTX PRO 6000 sur SGLang, et charge ses modèles sur le pod ;
3. ouvre un tunnel SSH vers le pod ;
4. ajoute les modèles du pod à Zed en tant que fournisseur distinct ;
5. exécute sept vérifications pour s’assurer que les modèles ne sont pas exécutés sur cette machine ;
6. arrête le pod ou le termine après une période pendant laquelle tous les GPU sont inactifs.

Grâce au module complémentaire, un agent planifie également des sessions en fonction d’un budget, conserve la mémoire du projet entre les opérations de compactage et les redémarrages, et exécute des files d’attente de transfert sans surveillance (voir [Le module complémentaire](#le-side-car-pour-les-agents)).

## La garantie et son fonctionnement

- **Le serveur de modèle est inaccessible, sauf via le tunnel.** Le pod exécute un moteur épinglé, Ollama (`ollama/ollama:0.35.0`) ou SGLang (`lmsysorg/sglang:v0.5.20-cu130`), lié à sa propre boucle locale, et le pod n’expose que `22/tcp`. Il n’y a pas de point de terminaison HTTP public à trouver ou à exploiter. Un modèle ne peut pas déplacer le moteur hors de la boucle locale.
- **Zed communique avec le tunnel, sur son propre port.** Le tunnel écoute sur `127.0.0.1:11435`. Votre Ollama local se trouve sur `11434`. offrig refuse de placer le tunnel sur `11434`, de sorte qu’un tunnel inactif ne peut pas se connecter au serveur local : la requête échoue.
- **Zed ne change jamais de fournisseur.** Les modèles du pod sont un fournisseur `offrig` distinct dans Zed. Si le pod est hors ligne, le choix de l’un d’entre eux génère une erreur ; Zed n’essaie pas un autre fournisseur.
- **Les poids n’existent jamais localement.** Les modèles sont extraits sur le pod, par le pod (ou téléchargés à partir de Hugging Face, ou lus à partir d’un volume réseau préparé).

Le programme de vérification vérifie cela à chaque fois, à partir des faits qu’offrig peut observer :

| Vérification | Échec lorsque |
|---|---|
| Le tunnel évite le port local d’Ollama | le port du tunnel est 11434 |
| Zed envoie les modèles de pod via le tunnel | L’URL du fournisseur de Zed est différente du tunnel |
| Ollama du pod n’est pas exposé à Internet | le pod mappe le port 11434 publiquement |
| Le tunnel se termine au niveau du pod | la liste des modèles via le tunnel diffère de la liste lue sur le pod via SSH |
| Les modèles de pod ne se trouvent pas sur cette machine | un modèle de pod existe également dans l’Ollama local |
| Aucun modèle de pod ne partage de nom avec un modèle Zed local | un nom dans le fournisseur offrig se trouve également dans la liste Ollama locale de Zed |
| Chaque modèle proposé par Zed se trouve sur le pod | Zed propose un modèle que le pod n’a pas |

## Installation

Nécessite Windows avec OpenSSH (intégré), Zed si vous souhaitez que les modèles soient disponibles dans un éditeur, et un compte RunPod.

1. Téléchargez `offrig-<version>-windows-x64.zip` depuis [Releases](https://github.com/mcp-tool-shop-org/offrig/releases), vérifiez-le par rapport au `SHA256SUMS` de la version, et décompressez-le dans votre `PATH`. Il contient `offrig.exe` (l’interface en ligne de commande), `offrig-app.exe` (l’application) et `offrig-mcp.exe` (le module complémentaire).
Pour compiler à partir du code source : `cargo build --release`, avec Rust 1.98.1 (version fixée dans `rust-toolchain.toml`).
2. Placez votre clé d’API RunPod dans la variable d’environnement utilisateur `RUNPOD_API_KEY`.
3. Ajoutez votre clé publique SSH dans les paramètres du compte RunPod. offrig utilise `~/.ssh/runpod_rustline` si elle est présente, puis `~/.ssh/id_ed25519`.
4. Pour les agents, enregistrez le module complémentaire auprès de Claude Code au niveau de l’utilisateur : `claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Cela ouvre le stockage d’un projet uniquement lors de la première utilisation, il est donc inoffensif dans les projets qui ne l’utilisent jamais.

Le [manuel](https://mcp-tool-shop-org.github.io/offrig/handbook/) explique comment configurer un premier pod, le module complémentaire, la configuration, les canaux et les pods de tâches.

## Utilisation

**Application :** démarrez `offrig-app`, sélectionnez un profil, appuyez sur **Lancer le pod**. Une fois qu’il est prêt, les modèles apparaissent dans le panneau d’agent de Zed sous la forme « RunPod · … ». Redémarrez Zed une fois après le premier lancement pour qu’il reconnaisse `OFFRIG_API_KEY`.

**CLI :**

```text
offrig status                 balance, runway, pods
offrig gpus --count 2         live offers for a GPU count
offrig profiles               tiers and their models
offrig up medium              launch, pull, wire Zed, run the checks, hold the tunnel
offrig up frontier --wait 180  wait up to 3 hours for the GPUs, renting nothing meanwhile
offrig tunnel medium          hold the tunnel to a running pod
offrig check gpt-oss:120b     streamed chat with a tool call, the way Zed sends it
offrig guard                  run the seven checks
offrig pull <model>           pull another model onto the pod
offrig connect                open the pod's /workspace in Zed for remote editing
offrig down medium --yes      terminate the pod
offrig zed-remove             take the provider out of Zed
offrig budget 15              set this project's spending cap for agent sessions (human only)
offrig index src docs           index this project for hybrid search (CPU embeddings)
offrig verify calibrate gold.jsonl --model gemma4:31b   score a local verifier model
offrig stage frontier --dc EUR-IS-1 --yes   stage weights on a network volume (bills monthly)
```

`offrig --help` et `offrig <command> --help` répertorient toutes les commandes et options. Le bloc ci-dessus correspond à l’ensemble quotidien. `offrig init`, `offrig models`, `offrig zed` et `offrig up --no-zed` se trouvent dans le [manuel](https://mcp-tool-shop-org.github.io/offrig/handbook/usage/).

### Sortie, codes de sortie et erreurs

- **Niveaux de journalisation :** `-q` affiche uniquement les erreurs et les résultats d’une commande ; `-v` ajoute chaque appel RunPod et son horodatage ; `--debug` ajoute les corps de réponse ayant échoué et les chaînes d’erreurs complètes. La clé d’API est masquée à chaque niveau.
- **Codes de sortie :** `0` succès, `1` quelque chose à corriger de votre côté (arguments, configuration, refus d’un contrôle ou d’un budget, clé manquante), `2` une erreur d’exécution (RunPod, réseau, ssh, délai d’attente, capacité insuffisante).
- Les **erreurs du module complémentaire** sont des résultats, jamais des erreurs de protocole : `ok:false` avec un `code` stable, le texte `error`, un `next_action` et un `retryable`. Les codes sont répertoriés dans la [référence du manuel](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## Le side-car (pour les agents)

`offrig-mcp` est un serveur MCP qu’un agent tel que Claude Code utilise comme outil. Il conserve une base de données par projet à l’adresse `<project>/.offrig/offrig.db` qui survit à tous les pods, de sorte qu’une session survit à la compaction ou à un redémarrage sans avoir à tout expliquer à nouveau.

| Outil | Ce que cela fait |
|---|---|
| `offrig_status` | Le projet, le budget, le solde RunPod et la marge de manœuvre, les pods offrig, chaque plan ouvert avec son `plan_id`, le nom du canal et du pod, la file d’attente de transfert avec les tâches obsolètes signalées. Chaque pod comprend `gpu_count` et `gpus` (`2x <type>`) de l’enregistrement RunPod, ainsi que le `planned_gpu_count` du plan. |
| `offrig_offers` | Offres de GPU en direct pour un nombre de GPU |
| `offrig_plan` | Calcule le coût d’une session dans le pire des cas (prix en direct x (heures maximales + 780 s de délai d’arrêt automatique), arrondi au centime près) ; refusé si le budget restant est insuffisant. La réponse indique le `ssh_alias` du canal et le `pod_name` que le lancement créera, ainsi que le `container_disk_gb` qu’il demandera (l’option `container_disk_gb` remplace celle du profil ; voir « Disque du conteneur »). Les options `max_price_hr` et `no_fallback` facultatives limitent les GPU qu’il peut louer (voir « Fixer le matériel d’un plan ») ; l’option `wait_minutes` facultative définit la durée pendant laquelle le lancement tente de relancer l’opération lorsqu’il n’y a pas de capacité disponible (voir « Attendre la capacité »). |
| `offrig_memory_search` | Recherche dans la mémoire du projet actif, chaque résultat avec source et date |
| `offrig_memory_record` | Ajoute une brève description, une contrainte, une décision, un fait ou un point de contrôle ; les modifications sont des remplacements avec une justification |
| `offrig_handoffs` | Met en file d’attente les transferts de tâches (chacun nécessite une vérification d’acceptation ; vérifications déterministes facultatives), les liste, prévisualise les blocs de tâches, affiche la meilleure sortie d’un transfert (également écrite dans `.offrig/out/`), enregistre les résultats (complet, non valide, violation, échec, nouvelle tentative avec commentaires) |
| `offrig_launch` | **Dépense.** N’accepte qu’un `plan_id` : valide le pire des cas, attend la location des GPU sans rien faire, démarre le pod, ouvre le tunnel, télécharge les modèles, démarre le programme de surveillance. Idempotent par plan. Refusé si le contexte a déjà un plan ou un pod actif : `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | Progression du lancement (pendant le démarrage du pod, l’étape est dérivée de l’état du pod au moment de l’appel, et `progress.pod_status` est le statut brut v2 ; chaque nouvelle tentative de capacité est comptabilisée dans `progress.capacity_wait`), le type de GPU, `gpu_count`, `gpus`, `planned_gpu_count`, et une fois que ssh est opérationnel, `measured_gpu_count` (une lecture sans cartes est `0`). `gpu_mismatch` est vrai lorsque l’enregistrement ou la mesure diffère du plan. La version CUDA de l’hôte est mesurée avec `nvidia-smi`, avec une entrée `warnings` bruyante lorsque l’hôte est plus ancien que la limite CUDA du plan. Pour une recette, `ready` signifie qu’une complétion a renvoyé un choix. `engine_hung` et `engine_unresponsive` maintiennent le pod en marche. Surveillance de la disponibilité, minutes restantes, dépenses actuelles |
| `offrig_ask` | Une itération d’un transfert sur le modèle du pod, le contexte étant construit à partir du stockage du projet ; la réponse est renvoyée en tant que sortie non fiable |
| `offrig_run` | Démarre un exécuteur distinct qui maintient chaque emplacement de modèle actif : prépare chaque transfert prêt, révise au maximum deux fois en cas d’échec des vérifications, transmet les résultats aux transferts dépendants, puis arrête le pod lorsque la file d’attente est vide (sauf si `keep_pod`). Les tâches que le code ne peut pas vérifier sont mises en attente pour examen. |
| `offrig_put` | Copie un fichier ou un répertoire local vers un pod de tâche (scp) ; les chemins relatifs du pod se trouvent sous `/workspace/job`. `plan_id` facultatif (voir ci-dessous) |
| `offrig_exec` | Exécute une commande bash sur un pod de tâche, en mode distinct afin qu’elle survive au side-car (`start`), signale l’exécution ou la fin avec son code de sortie et la fin du journal (`status` ; `save_log` copie également l’ensemble du journal dans un fichier local), la tue (`stop`) ou exécute une commande courte maintenant et renvoie sa sortie standard, son erreur standard et son code de sortie (`run`, `timeout_secs` par défaut 30, au maximum 120). `plan_id` facultatif (voir ci-dessous) |
| `offrig_get` | Copie un fichier ou un répertoire du pod de tâche, en créant les dossiers parents locaux manquants ; effectuez cette opération avant l’arrêt, qui supprime le disque du pod. `plan_id` facultatif (voir ci-dessous) |
| `offrig_shutdown` | **Détruit le pod.** Le termine et clôt les comptes du plan avec les dépenses mesurées ; refusé si des transferts sont en cours, sauf si une raison est fournie. Supprime le bloc `~/.ssh/config` du contexte lorsque celui-ci nomme ce pod (`ssh_block_removed`) |
| `offrig_complete` | **Dépense.** Une complétion OpenRouter, pour un projet et un modèle autorisés uniquement. Valide le pire des cas par rapport au budget, écrit la réponse dans un fichier de projet (en remplaçant ce chemin s’il existe déjà) et enregistre la charge réelle. |

**Sur quel plan de tâche un outil de tâche agit-il.** `offrig_put`, `offrig_exec` et `offrig_get` acceptent un `plan_id` facultatif. S’il n’y a qu’un seul plan de tâche ouvert et aucun `plan_id`, ils l’utilisent, comme auparavant. S’il y a plus d’un plan de tâche ouvert et aucun `plan_id`, ils refusent et listent les plans ouverts (ID, profil, nom du pod) : ils ne devinent jamais. Avec un `plan_id`, ils agissent uniquement sur le pod de ce plan et uniquement après avoir vérifié que le nom du pod est celui que ce plan possède (le pod d’un autre contexte est refusé, et pas seulement celui d’un autre plan). Chaque réponse de l’outil de tâche et `offrig_job` indique le `project` et le `plan_id` sur lesquels il a agi (les réponses de l’outil de tâche indiquent également le `lane`) ; `offrig_status` indique le `project` et liste tous les plans ouverts avec leur `plan_id`, leur contexte et leur nom de pod.

**Tâches et skills.** Un client qui prend en charge MCP Tasks peut exécuter `offrig_launch` comme tâche : `tasks/get` affiche le même état que `offrig_job`, et `tasks/cancel` annule un lancement encore en cours (via `offrig_shutdown`) ; pour arrêter une session prête, appelez `offrig_shutdown`. L'échéance du plan et le watchdog décident toujours du moment où un pod s'arrête. Le serveur publie aussi une skill de procédure, `offrig-session`. Les détails se trouvent dans le [manuel du side-car](https://mcp-tool-shop-org.github.io/offrig/handbook/side-car/#tasks).

Les rôles proviennent de Role OS (dossiers et cartes de démarrage) ainsi que de quatre rôles de jeu intégrés ici dans les formats de Role OS : concepteur de jeu, concepteur de systèmes, concepteur narratif, conservateur de l’histoire. Le plafond du budget est fixé uniquement par un humain :

```text
offrig budget --provider runpod 30       RunPod's cap (run in the project directory)
offrig budget --provider openrouter 8    OpenRouter's cap
offrig budget --show                     every cap, beside each provider's own balance
```

Chaque lancement de side-car démarre un **watchdog** : un processus distinct qui termine le pod à la date limite du plan (temps alloué + nombre maximal d’heures), même si l’agent, la session ou le side-car ont disparu. Il n’agit jamais sur une recherche infructueuse, se termine exactement une fois, clôt les comptes et enregistre les données dans `.offrig/watchdog-<plan>.log`. Si la préparation d’un pod loué échoue avant que le moteur ne soit opérationnel, le side-car le termine au lieu de le laisser facturer. `engine_hung` et `engine_unresponsive` sont uniquement des rapports sur chaque surface : le side-car, l’interface de ligne de commande et l’application maintiennent le pod actif et n’affichent pas le message « prêt ». Sur l’interface de ligne de commande et l’application, la commande s’arrête avec une erreur. Sur le side-car, le résultat du lancement reste positif, de sorte qu’aucun élément ne supprime le pod, le plan reste alloué et la tâche reste `running`. Un processus de moteur qui s’est déjà terminé n’est pas pris en compte dans ce rapport. Sur le side-car, il s’agit d’un lancement ayant échoué, et le compensateur supprime le pod. Un pod démarré avec `offrig up` ou l’application n’a pas de watchdog. Lorsque la commande s’arrête avec une erreur (blocage, moteur non réactif, moteur arrêté ou dépassement du délai), le pod continue de facturer jusqu’à ce que `offrig down <profile> --yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED` soit l’exception : dans ce cas, il est terminé sur toutes les surfaces.

La conception et les preuves qui la soutiennent se trouvent dans [docs/sidecar-design.md](docs/sidecar-design.md).

### Index du projet et vérificateur

```text
offrig index src docs                 chunk this project; only changed files are re-embedded
offrig index --status                 counts, model, dimension; changes nothing
offrig index --rebuild                drop every vector and embed again
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on
offrig verify calibrate --think-policy grounded=off,reasoning=escalate:off>on --model gemma4:31b gold/grounded.jsonl
offrig verify calibrate --report-only .offrig/out/calibrate-<model>-<time>
```

`offrig index` est intégré avec **EmbeddingGemma 2** par Google DeepMind (Apache-2.0, non limité), et est exécuté sur le CPU par [`tools/embed-serve`](tools/embed-serve/) (`embed_url`, par défaut `http://127.0.0.1:11490`), et non sur le serveur partagé sur 11434, ni sur un GPU. Il a remplacé nomic-embed-text dans la version 1.2.0 après un test de référence préenregistré sur les propres requêtes du studio (dense nDCG@10 +0,112) ; un index créé par Nomic est refusé jusqu’à `offrig index --rebuild`. Une fois que le projet dispose d’un index, `offrig_memory_search` passe par défaut à un mode hybride (mots-clés plus signification). `offrig verify calibrate` évalue un modèle de boucle locale comme vérificateur, avec une seule affirmation validée par appel, et avec les propres preuves de cette affirmation. Un nom de modèle contenant le mot « cloud » est refusé. `--num-ctx` passe par défaut à `auto` et définit la taille d’une fenêtre pour l’exécution. `--keep-thinking` écrit le texte de réflexion dans `thinking.jsonl`, uniquement dans le répertoire d’exécution. `--jobs <N>` (de 1 à 8, par défaut 1) conserve jusqu’à N affirmations en cours et les valide toujours dans l’ordre des fichiers : le serveur doit autoriser N affirmations en parallèle (`OLLAMA_NUM_PARALLEL`, ce qui est par défaut 1 dans Ollama), sinon les requêtes supplémentaires sont mises en file d’attente, la mémoire KV requise est proportionnelle au nombre de slots parallèles, et la concurrence peut modifier les résultats jusqu’à ce qu’ils soient mesurés. La [référence du manuel](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/) liste les indicateurs.

### Voies : un side-car par projet, pas de collisions

Deux projets peuvent exécuter des processus auxiliaires en même temps sur un seul compte RunPod. Chaque projet dispose de sa propre **tâche** : un alias SSH, un port de tunnel et une étiquette de nom de pod que aucun autre projet ne partage.

| | Voie standard (l’interface de ligne de commande, l’application, Zed) | Voie d’un projet |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Port du tunnel | `11435` (exécuteur `11436`) | premier port disponible de `11500`, `11502`, ... (exécuteur : le port ci-dessus) |
| Port du side-car (pilote shell) | aucun | `11700` + l’emplacement de la voie : `11700`, `11701`, ... |
| Nom du pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Bloc SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` provient du nom du dossier du projet (`aspire-si`, `ai-jam-sessions`), avec un hachage court ajouté lorsque deux projets partagent le même nom de dossier. La voie d’un projet est allouée la première fois qu’il planifie une session, elle est écrite dans `lanes.toml` dans le répertoire de configuration d’offrig et est conservée : le même projet obtient la même voie après chaque redémarrage. L’allocation utilise un fichier de verrouillage et écrit le registre de manière atomique, de sorte que deux side-car démarrés simultanément ne partagent jamais une étiquette, un alias ou un port. Aucune voie ne peut être `11434` (le port d’Ollama local) : la plage commence à `11500`, et un registre modifié pour indiquer autre chose est refusé. Un plan enregistre sa voie, et son lancement, son exécuteur, son watchdog et son arrêt utilisent tous cette voie, et non la configuration globale.

Un side-car ne fait que faire correspondre, lister ou arrêter les pods portant le nom de sa propre voie. La voie d’un autre projet, les pods de la voie standard `offrig-<profile>` et tout autre pod du compte sont laissés de côté : la vérification du lancement pour un pod actif ne demande que sa propre voie, l’arrêt refuse un pod dont le nom n’est pas celui de la voie du plan, et la vérification des pods orphelins du tunnel tue un pod `ssh` obsolète uniquement lorsque son transfert et son alias sont ceux de la voie. Les plans créés avant l’existence des voies n’ont aucune voie enregistrée et continuent de fonctionner sur la voie standard, de sorte qu’un pod lancé selon l’ancien schéma est arrêté par le même plan qui l’a démarré.

**Le propre port du side-car.** `offrig-mcp` communique via MCP sur stdio. Un pilote shell qui le maintient ouvert pendant toute une session (lorsque la propre connexion MCP de la session est inactive) le place derrière un port HTTP en boucle, et ce port était autrefois un numéro unique pour toute la machine (`11439`) : un deuxième pilote de projet, ou tout autre programme, pouvait le prendre et le premier side-car s’éteignait sans un mot. La valeur par défaut est maintenant par projet, à partir de la voie du projet, de la même manière que le port du tunnel : l’emplacement de la voie `i` (port du tunnel `11500 + 2i`) obtient le port du side-car `11700 + i`. La plage `11700` à `11763` se trouve au-dessus de chaque port de tunnel et d’exécuteur qu’une voie peut avoir (`11500` à `11627`), de la voie standard `11435` et `11436`, et d’Ollama local `11434`, de sorte qu’un port de side-car ne puisse jamais être un port de tunnel. Rien de nouveau n’est stocké : `lanes.toml` n’est pas modifié et le port découle de la voie. `OFFRIG_SIDECAR_PORT` continue de le remplacer ; une valeur qui n’est pas un port, qui est inférieure à 1024, ou qui est `11434`, `11435`, `11436` ou toute autre valeur dans la plage du tunnel de la voie est refusée.

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Avec `--check`, un port déjà utilisé est une erreur qui indique le port et, lorsqu’un conteneur annexe « offrig » y répond, le projet qu’il prend en charge : `le port 11700 du conteneur annexe est déjà utilisé : un conteneur annexe offrig prend déjà en charge le projet <path> ; arrêtez-le d’abord, ou définissez OFFRIG_SIDECAR_PORT sur un port libre`. La vérification interroge la manière dont le pilote répond déjà (une requête en tant que projet que personne ne prend en charge, ce que le pilote refuse avant de toucher à un outil), elle ne change donc rien dans un conteneur annexe en cours d’exécution. `offrig_status` signale la « voie » `sidecar_port`.

**Une voie, un seul pod actif.** Une voie a un alias SSH et un port de tunnel, elle sert donc un seul pod à la fois : un deuxième pod dans la voie (`offrig-<tag>-job` à côté de `offrig-<tag>-jam`) réaffecterait l’alias à lui-même et enverrait le `offrig_put`, `offrig_exec` et `offrig_get` du premier plan à la mauvaise machine. `offrig_launch` refuse donc tant que la voie a un plan ouvert ou tout pod actif qu’elle possède, avec `lane <tag> has a live pod <name> (plan <id>); shut it down first`, before anything is committed or rented. The plain lane's `offrig up` et l’application refuse de la même manière pour un pod d’un autre profil (le pod du même profil est toujours réutilisé). Fermez d’abord le premier plan, puis lancez le suivant.

## Niveaux

Les profils sont stockés dans `%APPDATA%\offrig\config.toml` (écrits lors du premier changement). Valeurs par défaut :

| Profil | GPU | Modèles | Coût typique |
|---|---|---|---|
| petit | 1 × RTX 2000 Ada / GPU de classe A4000 | `qwen3:4b` | environ 0,25 $/heure |
| moyen | 1 × RTX PRO 6000 (96 Go) ; A100 ou H100 80 Go s’il n’y en a pas de libre | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | 2,09 $/heure (fallback A100 1,59 $) |
| frontier | 4 × RTX PRO 6000 (384 Go), **SGLang** | Qwen3-Coder-480B AWQ 4 bits (252 Go), environ 130 Go restants pour le contexte | 8,36 $/heure |
| avant-garde-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 Go) : le chemin du moteur d’avant-garde, testé à moindre coût | environ 1,7 $/heure |
| avant-garde-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 Go) : les noyaux MoE 4 bits de l’avant-garde, testés à moindre coût | environ 1,7 $/heure |
| tâche | 1 × RTX PRO 6000 (96 Go) ; A100 ou H100 80 Go s’il n’y en a pas de libre | aucun : un **pod de tâche** exécute votre travail, et non un serveur de modèle | 2,09 $/heure (fallback A100 1,59 $) |
| en attente | 1 × A40 (48 Go) en premier ; A6000, A5000, 3090, L4 ou 4090 s’il n’y en a pas de libre | aucun : un **pod de tâche** pour les rendus de chant d’ai-jam-sessions (SoulX-Singer) | 0,49 $/heure (A40) |

Un profil avec un `recipe` exécute un autre moteur qu’Ollama : une image épinglée (`lmsysorg/sglang:v0.5.20-cu130`), un modèle Hugging Face qu’il télécharge au démarrage et des arguments de serveur supplémentaires. offrig définit le parallélisme des tenseurs à partir du nombre de GPU, la longueur du contexte à partir du profil et maintient le moteur sur la boucle locale du pod ; une recette ne peut pas remplacer ces paramètres. Pour un dépôt protégé, `hf_token_secret` indique un secret RunPod, référencé sous le nom de `{{ RUNPOD_SECRET_<name> }}`, afin que le jeton n’entre jamais dans la spécification du pod. Le lancement signale les poids sur le disque pendant que le moteur les télécharge, et s’arrête immédiatement (avec le journal du moteur) si le moteur se termine. Il ne signale pas que le moteur est prêt lorsque `/health` répond, et le fait que `/v1/models` réponde n’est pas non plus un signe de préparation. Après que `/health` renvoie 200, offrig envoie une complétion de chat (`max_tokens` 1, `temperature` 0) et attend jusqu’à 30 secondes. L’étape n’est `ready` que si cette réponse contient un choix. Si la complétion échoue ou dépasse le délai, offrig réessaie une fois après 15 secondes. Un deuxième échec est l’étape `engine_unresponsive` : la tâche continue de s’exécuter, et l’erreur et le journal du moteur se trouvent dans la tâche. La prochaine action est : « le moteur ne répond pas aux complétions : arrêtez le pod, ou attendez s’il est encore en phase de démarrage ». offrig n’arrête pas le pod pour cela, ni sur le conteneur annexe, ni sur la CLI, ni sur l’application. Une première requête lente (capture de graphique, JIT) n’est pas une raison pour abandonner le téléchargement et le chargement ; le délai du plan limite toujours le coût. Le minuteur de blocage démarre à partir de `grep -c` de `Application startup complete` lors de la même lecture SSH que le suivi du journal, sur l’ensemble du journal du moteur, et pas seulement sur les 40 dernières lignes. Un moteur bavard peut faire sortir la ligne de démarrage de ce suivi. Le comptage démarre toujours le minuteur. Si ce comptage est d’au moins un et que `/health` reste en panne pendant 3 minutes, l’étape est `engine_hung` : ces 40 dernières lignes, le nombre de secondes écoulées depuis que le comptage a été observé pour la première fois, et la prochaine action `shut the pod down: the engine finished startup but never became healthy`.
offrig signale ce blocage et laisse le pod en marche. L’arrêter vous incombe, tout comme un décalage du nombre de GPU. Un avertissement sur le nombre de GPU n’est pas ajouté à la prochaine action, ni à celle qui ne répond pas, de sorte que la phrase reste exacte. Un processus de moteur qui s’est déjà terminé n’est pas non plus signalé. Sur le conteneur annexe, le compensateur supprime ce pod. Sur la CLI et l’application, la commande s’arrête avec l’erreur et ne le supprime pas.

Chaque profil répertorie les types de GPU par ordre de priorité. v2 sélectionne un type et ne revient pas en arrière,
par conséquent, offrig essaie lui-même cette liste, dans l’ordre, et ne dépasse jamais le prix du plan. Lorsqu’aucun n’est
disponible, un profil peut attendre (`wait_for_gpu_minutes` ; l’attente maximale est de 120 minutes) : offrig
vérifie chaque minute et crée le pod dès que les GPU sont disponibles. Rien n’est loué pendant
cette période d’attente. Ctrl+C ou l’annulation du lancement de l’application l’arrêtent, et si le catalogue en direct est hors service, il
réessaie simplement de créer le pod chaque minute. Les configurations multi-GPU importantes se mettent en place et disparaissent en quelques minutes.
Les prix correspondent aux prix de la liste sécurisée du catalogue v2 (`GET /catalog/gpus`), par GPU, multipliés
par le nombre de GPU. Une entrée NONE n’est pas gratuite. La page publique des prix n’est pas le prix disponible. `CUDA_VERSIONS` n’est qu’une solution de repli lorsque la liste `cudaVersions` d’un GPU est omise.

### En attente de capacité

Un plan limité avec `no_fallback` ou `max_price_hr` manque souvent de capacité, de sorte que le lancement réessaie silencieusement au lieu d’échouer, sans rien louer pendant ce temps. La durée de l’attente est, par ordre : le `wait_minutes` du plan (un argument `offrig_plan`, stocké avec le plan ; `0` échoue immédiatement), sinon le `wait_for_gpu_minutes` du profil. Le profil `job` a une valeur par défaut de 20 minutes. L’attente est réduite au temps qu’il reste au plan, moins une réserve de cinq minutes, de sorte qu’il ne dépasse jamais la date limite du plan, et comme rien n’est loué pendant l’attente, cela n’ajoute rien au pire des cas alloué. `offrig_launch` signale `capacity_wait_minutes` ; pendant l’attente, `offrig_job` affiche `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) et une étape qui indique de quelle vérification il s’agit. Lorsque l’attente est terminée, le lancement échoue avec `no capacity` et rien n’a été loué.

### Définition du matériel d’un plan

Un profil répertorie les types de GPU par ordre de priorité et RunPod sélectionne le premier qui dispose de capacité, de sorte que
sans limites, un plan peut se retrouver avec une carte de secours dotée de moins de mémoire, d’un pilote plus ancien et d’un
prix différent. Trois limites permettent de limiter un plan au matériel que son travail peut utiliser. La planification reste
gratuite ; les limites ne font que restreindre ce que le plan peut louer.

| Limite | Où | Effet |
|---|---|---|
| `min_cuda` | profil (`config.toml`) | La version CUDA la plus ancienne de l’hôte, provenant de la liste RunPod (`13.2`, `13.1`, `13.0`, `12.9`, ... `11.8`). La création du pod envoie cette valeur minimale en tant que `gpu.minCudaVersion`, de sorte qu’un hôte plus récent que la version copiée la plus récente est toujours pris en compte. Lorsque le catalogue renvoie `cudaVersions` pour un GPU, un plan conserve ce type uniquement si une version est disponible à cette valeur minimale ou au-dessus. Lorsque le catalogue omet la liste, la liste copiée est utilisée comme solution de repli. Pour un profil de tâche, la version la plus récente de cette liste et de la liste `[profiles.job] min_cuda` de l’image est appliquée. |
| `min_vram_gb` | profil | La quantité minimale totale de VRAM (tous les GPU du profil combinés) qu’un plan accepte. Les offres inférieures à cette valeur sont rejetées ; un type RunPod pour lequel aucune mémoire n’est répertoriée est également rejeté. |
| `max_price_hr` | argument `offrig_plan` | Le coût maximal du pod, en dollars par heure pour toutes ses GPU (le chiffre `offrig_offers` l’indique). Les offres supérieures sont rejetées, de même que les types sans prix indiqué (il ne peut pas être soumis à une limite). |
| `no_fallback` | argument `offrig_plan` | Seule la première famille de GPU du profil est autorisée. Les deux éditions RTX PRO 6000 Blackwell (Serveur et Station de travail) constituent une famille ; toutes les autres cartes, y compris les A100 SXM et PCIe, constituent leur propre famille. |

Les champs du profil sont facultatifs et par défaut non définis, de sorte qu’un `config.toml` écrit par un
programme précédent est chargé sans modification. Le profil `job` définit `min_cuda = "13.0"`.

`cuda_versions` est une liste exacte facultative en plus de `min_cuda`, par exemple
`cuda_versions = ["13.0"]`. Lorsqu’elle est définie, la création du pod envoie cette liste en tant que
`gpu.allowedCudaVersions`, dans l’ordre dans lequel elle est écrite, et n’envoie pas non plus de valeur minimale.
`offrig_plan` signale la liste. La définition de cette liste conjointement avec `min_cuda`, y compris la valeur minimale propre à une image de tâche, est refusée : les deux combinées sont ambiguës. Chaque version doit être une version qu’offrig accepte. Cette liste, de la plus récente à la plus ancienne, est 13.2, 13.1, 13.0, puis 12.9 jusqu’à
11.8. 13.2 et 13.1 sont antérieurs à l’énumération v1 documentée de RunPod (lue le 2026-10-11, qui s’arrête à 13.0). Elles sont répertoriées parce qu’un pod d’écriture du 2026-10-11 a été placé sur un
hôte 13.2. Une valeur minimale envoie toujours uniquement `gpu.minCudaVersion`.

`offrig_plan` calcule le pire des cas à partir de ce qui reste :
`(max_hours + 780 s) x min(max_price_hr, the dearest listed price among the remaining GPUs)` (arrondi au centime près ; 780 s est la marge d’arrêt automatique de 120 s dans le pod, plus 660 s de durée de nouvelle tentative). Sans
`max_price_hr`, il s’agit du prix le plus élevé répertorié dans le profil, comme auparavant. Un plan pour lequel il ne reste plus rien est refusé avec la raison de chaque GPU rejeté, et rien n’est écrit.

Le plan stocke la liste des GPU disponibles, ainsi que la limite minimale de CUDA ou la liste exacte de CUDA, et `offrig_launch` ne loue que ce qui est stocké dans le plan, jamais à partir d’une modification ultérieure
du profil. `offrig_job` et le résultat du lancement du processus secondaire
indiquent le type de GPU et la version CUDA de l’hôte réellement louée, dans un objet `rented`. L’API du pod n’indique pas la version CUDA de l’hôte, donc une fois que ssh est opérationnel, le lancement exécute
`nvidia-smi` une seule fois et la lit dans l’en-tête (`CUDA Version: 12.8`, ou
`CUDA UMD Version: 13.4` pour les pilotes plus récents) ; `rented.cuda_source` indique `nvidia-smi` ou
`pod API`. Si la version CUDA de l’hôte est antérieure à la limite minimale du plan, ou si le prix est supérieur à celui du plan, `offrig_job` renvoie une entrée `warnings`
et démarre `next_action` avec `WARNING`. Rien n’est arrêté automatiquement : l’arrêt de la location dépend de l’appelant (`offrig_shutdown`). Si ni l’API du pod ni `nvidia-smi`
ne fournissent une version CUDA, `rented.notes` l’indique et la limite minimale n’est pas vérifiée.

### Disque du conteneur

Un pod dispose de deux disques : le disque du conteneur, local à l’hôte, et le volume monté à
`/workspace`. Sur certains hôtes, `/workspace` est un système de fichiers réseau lent : un pod de tâche a mesuré
32 Mo/s, contre 354 Mo/s sur son disque de conteneur, et n’a pas pu récupérer environ 130 Go de
modèles à temps, alors que le disque du conteneur n’était que de 60 Go. La taille du disque du conteneur est la
valeur `container_disk_gb` du profil (de 30 à 60 Go dans les profils intégrés ; le profil `job` a
60 Go) et est transmise au pod lors de sa création sous la forme `disk`. `offrig_plan` prend
`container_disk_gb` (de 1 à 2000) pour la remplacer pour un plan ; le plan la stocke, le lancement
la transmet, et la réponse du plan et `offrig_status` affichent la taille en vigueur.

- Le disque du conteneur n’est pas facturé : offrig facture uniquement le temps d’utilisation du GPU, de sorte que le pire des cas du plan est le même quelle que soit la taille. RunPod facture le disque ; on n’a pas vérifié ici si le tarif qu’il indique pour le pod (`offrig_job` l’affiche) inclut le disque du conteneur.
- offrig ne déplace pas vos téléchargements. Les commandes de tâche commencent par `HF_HOME` sur le volume `/workspace` (`/workspace/hf`) ; pour utiliser le disque du conteneur, définissez le vôtre (`HF_HOME=/root/hf python ...`) dans la commande.
- Le disque du conteneur est supprimé avec le pod, comme le volume sans volume réseau : copiez les résultats avec `offrig_get` avant `offrig_shutdown`.
- La limite de 1 à 2000 est une vérification de cohérence propre à offrig pour éviter une erreur de frappe ; la limite réelle de RunPod n’est pas vérifiée.

### Pods de tâche

Un profil avec un `job` loue un GPU pour une tâche qui s’exécute dessus, comme une exécution d’entraînement, plutôt que pour servir un modèle. Son pod exécute une image PyTorch fixe (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 pour Blackwell) avec sshd et rien d’autre :

- Il ne sert aucun modèle, il n’y a donc pas de tunnel et rien n’est connecté à Zed. sshd ne permet aucun transfert (`AllowTcpForwarding=no`) ; la seule façon d’y accéder est de se connecter au pod via ssh.
- Un profil de tâche ne liste aucun modèle et ne peut pas non plus avoir de recette ; la vérification de configuration refuse les deux.
- `offrig up` et l’application refusent un profil de tâche avant de louer quoi que ce soit. Il s’exécute via le side-car : `offrig_plan profile=job`, `offrig_launch`, puis `offrig_put`, `offrig_exec` et `offrig_get`. Le lancement est prêt lorsque sshd répond.
- Une commande s’exécute en arrière-plan sur le pod (`setsid nohup`) dans `/workspace/job`, de sorte qu’elle dure plus longtemps que le side-car et la session ssh. Elle est envoyée sous forme de base64, de sorte que rien dans cette commande n’est lu par le shell ssh. Son journal et son statut de sortie restent dans `/workspace/offrig/jobs/`. Les téléchargements de Hugging Face sont envoyés dans `/workspace/hf` sur le volume du pod.
- `offrig_exec action=run` est destiné aux vérifications rapides (`ls`, `nvidia-smi`), pas aux tâches : il exécute la commande jusqu’à son terme sous `timeout` (par défaut 30 s, maximum 120 s) et renvoie `stdout`, `stderr`, `exit_code` et `timed_out`. La sortie est tronquée aux 64 Ko de chaque flux (`truncated`) et est une sortie de pod non fiable. Une commande qui nécessite plus de temps est une `start`.
- La fin du journal d’une tâche affiche des barres de progression réduites : les redessins de type tqdm, joints par des retours chariot, n’affichent que leur dernier cadre. `offrig_exec action=status save_log=<local path>` copie également l’ensemble du journal de la tâche, tel qu’il a été écrit, dans un fichier local (les dossiers parents sont créés), de sorte que la fin du journal puisse rester courte.
- L’image est une version CUDA 12.8, un profil de tâche indique donc la version CUDA la plus ancienne sur laquelle il s’exécute (`min_cuda = "12.8"`), et le pod est créé avec cette valeur minimale en tant que `gpu.minCudaVersion`. Sans cela, un hôte doté d’un pilote plus ancien démarre le pod et torch ne trouve pas de GPU après le début de la location. La tâche elle-même peut nécessiter plus que l’image : le profil `job` définit également `min_cuda = "13.0"` sur le profil (voir ci-dessus), car les tâches qu’il exécute installent une version actuelle de vLLM, dont la version PyTorch est une version CUDA 13.
- Le budget, le plan, le système de surveillance et l’arrêt fonctionnent comme pour tout autre profil. Copiez les résultats avant `offrig_shutdown` : le disque du pod est supprimé avec lui.
- `jam` est le profil de tâche qu’ai-jam-sessions utilise pour rendre ses chansons : SoulX-Singer nécessite beaucoup moins qu’une carte d’entraînement, il loue donc une carte bon marché de 24 à 48 Go. La configuration et la session se trouvent dans ce dépôt (`docs/vocal-offrig.md`) ; offrig ne sait rien sur le chant.

### Pondération en attente sur un volume réseau

Un profil de recette télécharge ses poids à chaque lancement : pour la version la plus récente, cela prend environ 20/22 minutes pour être prêt (252 Go, 8,36 $/h). La mise en place les place sur un volume réseau RunPod une seule fois :

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- Le volume est facturé mensuellement, qu’un pod s’exécute ou non (300 Go pour la version la plus récente coûtent environ 21 $/mois à 0,07 $/Go), donc seul un humain effectue la mise en place ; aucun outil d’agent ne peut le faire.
- Un volume se trouve dans un seul centre de données, de sorte que les pods du profil ne sont lancés que là-bas, et les offres et les plans sont tarifés là-bas. Choisissez-en un doté d’un stockage réseau et des GPU du profil ; la console de `offrig gpus` et de RunPod indique où ils se trouvent.
- Le téléchargement s’exécute sur le pod GPU le moins cher disponible dans ce centre de données. Le pod est terminé en cas de succès, d’échec ou de dépassement du délai d’attente.
- Le volume est enregistré dans le profil avant le début du téléchargement, de sorte qu’une mise en place ayant échoué n’est jamais oubliée ; relancez-la pour la reprendre, ou utilisez `--remove`.
- Un lancement mis en place exécute Hugging Face hors ligne, uniquement lorsque la mise en place est terminée (un marqueur sur le volume). Un volume partiellement mis en place télécharge le reste au lieu d’échouer.

## Sécurité financière

- Avant un lancement, offrig affiche la correspondance gratuite la moins chère et votre marge de manœuvre avec le pod en cours d’exécution. Si la marge de manœuvre est inférieure à une heure, il refuse, à moins que vous ne la validiez, car à zéro, RunPod arrête tous les pods du compte, y compris ceux qu’offrig ne gère pas.
- L’arrêt automatique termine le pod après 30 minutes si tous les GPU sont utilisés à moins de 5 % (configurable, ou désactivé).
- La fermeture de l’application avec un pod en cours d’exécution demande si vous souhaitez le terminer ou le laisser s’exécuter.
- offrig ne touche que les pods qu’il a nommés : `offrig-<profile>` pour l’interface de ligne de commande et l’application, `offrig-<tag>-<profile>` pour la voie side-car d’un projet (voir Voies). Un side-car ne touche jamais les pods d’une autre voie, les pods de la voie principale ou tout autre pod ; ceux-ci sont répertoriés, mais jamais modifiés.
- Pour les sessions d’agent, la limite est appliquée avant toute dépense : le pire des cas d’un plan (prix en direct × (nombre maximal d’heures + 780 s de fin d’exécution), arrondi au centime le plus proche) est pris en compte par rapport au budget défini par l’utilisateur et refusé s’il est dépassé, et un lancement ne prend qu’un ID de plan, de sorte qu’un agent ne peut pas définir son propre prix.
- Chaque lancement de side-car dispose d’un système de surveillance qui termine le pod à la date limite du plan, et le programmeur arrête le pod dès que sa file d’attente est vide.

## Ce que cela modifie sur votre machine

| Quoi | Où | Annuler |
|---|---|---|
| Fournisseur Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove` ; le premier original est conservé sous le nom `settings.json.offrig.bak` |
| Modèle par défaut de Zed (uniquement si vous le demandez) | même fichier | `offrig zed-remove` restaure le modèle par défaut précédent |
| `OFFRIG_API_KEY` (espace réservé ; Zed a besoin d’une clé) | environnement utilisateur | `setx OFFRIG_API_KEY ""` ou supprimez-le dans les propriétés du système |
| Alias SSH `offrig` | `~/.ssh/config`, entre les marqueurs `# >>> offrig:offrig >>>` | supprimer le bloc marqué |
| Alias SSH `offrig-<tag>`, un par projet lancé à partir d’un processus secondaire | `~/.ssh/config`, entre les marqueurs `# >>> offrig:offrig-<tag> >>>` | supprimer le bloc marqué |
| Chemins de projet | `%APPDATA%\offrig\lanes.toml` (chemin du projet, balise, alias, port de tunnel) | supprimer l’entrée du projet tant qu’aucun pod n’est en cours d’exécution dans son chemin, ou supprimer tout le fichier |
| Clés d’hôte du pod | `~/.ssh/known_hosts_offrig` | supprimer le fichier |
| Paramètres | `%APPDATA%\offrig\config.toml` | supprimer le fichier |
| Pondérations en attente (uniquement avec `offrig stage --yes`) | un volume réseau RunPod `offrig-<profile>` ; facturation mensuelle | `offrig stage <profile> --remove --yes` |

Les commentaires et la mise en page dans les paramètres de Zed sont conservés : les modifications passent par un arbre syntaxique JSONC.

## Modèle de menace

- **Clé d’API RunPod.** Lecture à partir de `RUNPOD_API_KEY` ; jamais écrite sur le disque ou dans les journaux. Le fournisseur Zed ne porte pas intentionnellement le nom `runpod`, car Zed lirait alors `RUNPOD_API_KEY` et l’enverrait au serveur de modèle.
- **Serveur de modèle.** Accessible uniquement via SSH avec votre clé. L’authentification par mot de passe est désactivée sur le pod, et sshd n’autorise que le transfert local.
- **Clés d’hôte.** Épinglées par point de terminaison dans un fichier distinct known-hosts. offrig oublie une clé uniquement lorsque le point de terminaison du pod change, car RunPod réutilise les paires adresse IP : port entre les pods.
- **Injection de code.** Les noms des modèles sont validés par rapport à la syntaxe des noms d’Ollama avant d’atteindre un shell distant.
- **Tunnels orphelins.** Si offrig se termine, son `ssh` peut continuer à conserver le port. Au prochain démarrage, offrig le termine, mais uniquement si l’écouteur transporte `ssh.exe` les spécifications de transfert exactes d’offrig. Tout autre élément sur le port est refusé et jamais terminé.
- **Pas de télémétrie.** offrig communique uniquement avec l’API de RunPod, votre pod et votre instance locale d’Ollama (pour comparer les listes de modèles).

## Tests

`cargo test --workspace` exécute plus de 250 tests, couvrant au moins 90 % des lignes (l’échec de la CI se produit en dessous) :

- **La bibliothèque principale :** analyse RunPod, spécifications des pods pour les deux moteurs, configuration SSH, modifications JSONC de Zed, règles de protection, logique de coût et d’inactivité, le stockage et ses migrations, rôles, assemblage de contexte, vérifications déterministes, décisions du moteur, le chien de garde et la mise en attente, y compris un RunPod factice qui prouve qu’une étape ayant échoué termine son pod.
- **L’application :** gestion de l’état plus tests d’interface utilisateur en mode clic dans l’environnement de test d’egui.
- **L’interface de ligne de commande :** codes de sortie, niveaux de journalisation et le fait que la clé d’API n’apparaît jamais dans la sortie.
- **Le processus secondaire :** de bout en bout via stdio par rapport à un RunPod factice, le processus réel du chien de garde et le processus réel du moteur par rapport à un modèle de pod factice ; chaque erreur d’outil est associée à un code.

`scripts/verify.sh` (ou `scripts/verify.ps1`) exécute la vérification du format, clippy, les tests et
une exécution de test de chaque binaire dans une seule commande. La CI exécute également `cargo deny`, une analyse OSV de
`Cargo.lock`, la couverture vers Codecov et `atlas check`.

### Enregistrement des tests en direct (2026-10-02, niveau moyen, A100 80 Go, environ 0,45 $)

- Le pod est opérationnel en environ 80 secondes ; sshd, le tunnel et l’Ollama 0.35.0 du pod répondent.
- 97 Go de modèles sont téléchargés sur le pod à environ 150 à 250 Mo/s.
- `qwen3-coder:30b-a3b-q8_0` et `gpt-oss:120b` ont chacun répondu à une conversation en continu avec un
appel d’outil correct via le tunnel. Ils ont utilisé 36 Go et 64 Go de la VRAM du pod ; l’Ollama local n’a rien chargé et n’était pas du tout sur le GPU local.
- Les sept vérifications de sécurité ont été réussies, à partir de la CLI et de l’application.
- Une CLI arrêtée de manière abrupte a laissé son `ssh` maintenir le port ouvert ; la prochaine exécution l’a récupéré.
- Le tunnel, les vérifications, le test de modèle et l’arrêt de l’application ont été effectués via ses boutons.

Bogues détectés lors de l’exécution en direct, maintenant corrigés et couverts : une liste `&&` en arrière-plan a maintenu le stdout de ssh ouvert et a bloqué le démarrage du téléchargement ; la liste des pods ne contenait pas les types de GPU sans
`includeMachine=true` ; la vérification de lancement a compté deux fois le prix d’un pod en cours d’exécution.

### Répétition du processus secondaire (2026-10-03, petite catégorie, RTX 2000 Ada, 0,08 $ réservé)

L’Ollama `offrig-mcp` installé est exécuté via stdio, comme un agent le ferait :

- `offrig_plan` a estimé 0,5 h à 0,15 $ dans le pire des cas ; `offrig_launch` l’a validé, a démarré le chien de garde, et un deuxième appel a renvoyé le même travail. Un pod a été loué, à 0,24 $/h.
- SSH est opérationnel 100 s après la location, `qwen3:4b` est téléchargé, prêt à 150 s.
- `offrig_ask` a exécuté un transfert de concepteur de jeu en 46 s ; la réponse a satisfait sa vérification d’acceptation et a conservé la contrainte de cinq points de la mémoire.
- Le pod a accédé à Internet (Wikipédia, API GitHub). Sur la base des données récupérées par le pod, le modèle a répondu correctement aux questions actuelles ; lorsqu’on lui a posé des questions à froid, il a déclaré qu’il n’avait pas d’accès en direct.
- `offrig_shutdown` à partir d’un processus secondaire fraîchement créé a terminé le pod et a clôturé les comptes ; le chien de garde a vu la fin du plan et s’est arrêté. Le GPU local est resté inactif tout au long du processus.

Détecté et corrigé : le pod ne traitait qu’une seule requête à la fois (`OLLAMA_NUM_PARALLEL=1`) ; quatre
emplacements ont traité 8 requêtes parallèles provenant de 40 à 102 jetons/s sur le même GPU, de sorte que chaque profil dispose maintenant de `parallel = 4`. `complete` a été refusé sans raison (il a maintenant une valeur par défaut de
« vérification d’acceptation réussie » ; les échecs doivent toujours en avoir une). L’état suggérait d’enregistrer un bref
moment pendant qu’une session était active. Le texte qui s’échappe dans une réponse est supprimé, et une réponse
vidée par la réflexion indique qu’il faut augmenter `max_tokens`.

### Répétition de l’exécution (2026-10-03, petite catégorie, RTX 2000 Ada, 0,04 $ réservé)

Cinq transferts, dont un dépendant de l’autre, ont fonctionné avec `offrig_run` sans personne pour les piloter :

- Quatre transferts en cours simultanément sur quatre emplacements (12,9 Go sur 16 Go de VRAM) ; le transfert dépendant a commencé dès que sa dépendance a été terminée et a été basé sur son résultat.
- Les trois transferts dont les vérifications couvraient l’acceptation ont été terminés par eux-mêmes ; les histoires rivales (vérifications partielles) et le récit (sans vérifications) sont passés en phase de révision.
- La révision a renvoyé le récit (« la rivière porte le nom du projet ») ; l’exécuteur en direct l’a adopté et l’a révisé en fonction des commentaires (« rivière Veyl »).
- La file d’attente a été vidée en 6,5 minutes (6 tours, 20 861 jetons) ; l’exécuteur a arrêté le pod
lui-même.

Appris : les vérifications déterministes vérifient la structure, et non la qualité de la conception. Le modèle 4 B a réussi « trois verbes » avec des verbes peu précis, de sorte que `accept_on_checks` est destiné au travail structurel et que le travail de conception est renvoyé pour examen. qwen3:4b a passé environ 4 000 jetons à réfléchir par tour, même sur trois lignes de récit. Une file d’attente maintient chaque emplacement occupé uniquement lorsqu’elle contient suffisamment de transferts indépendants ; une chaîne de dépendances s’exécute un par un.

### Exécutions Frontier et SGLang (2026-10-03, 4,27 $ réservé)

| Exécuter | Pod | Prêt après | File d’attente | Réservé |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 min | 4 transferts en 15 s | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 min | 4 transferts | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 min** (252 Go à 278 Mo/s, puis chargement) | **31 transferts en 20 s** | **$3.59** |
| Exploration de 30 milliards de paramètres | 1 × RTX PRO 6000 | 10 min (placement lent des pods) | exploration uniquement | $0.43 |

- SGLang v0.5.20 (cu130) s’exécute sur Blackwell : inférence rapide de l’attention, `awq_marlin` pour les
poids MoE à 4 bits, parallélisation des tenseurs via PCIe sur quatre cartes ; `/dev/shm` était de 352 Go.
- Le travail effectué sur le modèle de pointe était clairement supérieur à celui des petits modèles : dialogues réalistes et module Rust qui a été compilé et a réussi ses trois tests (vérifiés localement). La version 30 B FP8
de la même tâche n’a pas pu être compilée.
- Un seul modèle chargé suffit pour traiter un ensemble de requêtes ; aucune copie n’est nécessaire. Test de concurrence avec des réponses de 384 jetons, nombre total de jetons par seconde :

| Agents | 480B sur 4 GPU | 30 milliards de paramètres AWQ sur 1 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

La vitesse par agent diminue à mesure que des agents sont ajoutés (30 milliards de paramètres : 88 → 24 jetons/s à 64), mais le débit total continue d’augmenter ; les gains de 30 milliards de paramètres se stabilisent après 256. Le cache KV de frontier
contient 398 526 jetons, de sorte qu’avec de véritables contextes de transfert de 2 à 8 000 jetons, le niveau frontier
exécute désormais 64 transferts en parallèle et les niveaux à une seule carte SGLang, 32.

Problèmes détectés et corrigés en cours de route : les révisions ont augmenté la taille de la sortie pour passer un contrôle d’en-tête (il s’agit désormais d’un contrôle `no_repeats` intégré, et les révisions restructurent sur place) ; la sortie requise d’un rôle a été divulguée dans les livrables (le transfert définit désormais le format) ; le retour d’information de l’en-tête indique le format Markdown ; une révision inchangée s’arrête au lieu de se répéter.

Pas encore vérifié en direct : un chat
envoyé à partir du panneau d’agent de Zed lui-même (la forme de la requête utilisée par Zed est testée directement), et la vérification de la version 1.1.0, le rapport d’erreur et l’épinglage CUDA exact sur un pod loué (voir
[Statut](#statut)).

## Conformité aux normes

Évaluation par rapport aux normes de flux de travail du studio (0 manquantes, 1 partielles, 2 présentes, 3 exemplaires).

- **PIN_PER_STEP : 2.** Les images des pods sont épinglées aux étiquettes de version (`ollama/ollama:0.35.0`,
`lmsysorg/sglang:v0.5.20-cu130` ; une recette refuse `latest`), le compilateur à la version 1.98.1,
les dépendances par `Cargo.lock` et le moteur Atlas à la version 1.24.0 du parc. Chaque transfert
enregistre son modèle, son hachage de rôle et son hachage d’invite. Les modèles sont épinglés par étiquette ou par ID de dépôt,
et non par hachage.
- **ANDON_AUTHORITY : 3.** Chaque étape interrompt l’exécution en cas de défaut : un plan dont les poids
dépassent la capacité du disque est refusé avant toute dépense ; un échec de téléchargement arrête le lancement ; une modification Zed
qui ne peut pas être relue n’est pas écrite ; un fichier de paramètres défectueux est signalé, mais jamais
réécrit ; CI bloque sur fmt, clippy, tests, licences et avis.
- **NAMED_COMPENSATORS : 2.** Chaque action irréversible a une fonction d’annulation, répertoriée ci-dessous.
- **DECOMPOSE_BY_SECRETS : 2.** Un module par élément qui change pour ses propres raisons :
l’API de RunPod (`runpod`), le contenu du pod (`spec`), le transport (`tunnel`,
`remote`), chaque fichier local modifié (`sshconfig`, `zed`) et les règles (`guard`,
`cost`). Les interfaces avant n’ont aucune logique au-delà de la présentation.
- **UNCERTAINTY_GATED_HUMANS : 2.** offrig ne demande que là où le résultat est coûteux ou
avec perte : lancement avec moins d’une heure de temps disponible, arrêt d’un pod (avec indication de ce qui est perdu) et
arrêt avec un pod qui facture encore. Deux décisions appartiennent uniquement à un humain, et aucun
outil d’agent ne peut les prendre : le plafond budgétaire et la préparation d’un volume, qui facture mensuellement.
La sortie du transfert que le code ne peut pas vérifier est mise en attente d’examen au lieu d’être terminée.
- **EXTERNAL_VERIFIER : n/a.** Aucune revendication spécialisée.

**Compensateurs**

| Action | Annuler | État après annulation | Propriétaire |
|---|---|---|---|
| Créer un pod (démarre la facturation) | `offrig down <profile> --yes`, arrêt de l’application ou arrêt automatique | pod terminé, facturation arrêtée | l’opérateur exécutant offrig |
| Terminer un pod | aucun pour son disque ; relancer le profil et les modèles sont à nouveau téléchargés (un volume réseau les conserve) | nouveau pod, même profil | l’opérateur |
| Écrire le fournisseur Zed ou le modèle par défaut | `offrig zed-remove`, ou restaurer `settings.json.offrig.bak` | Zed comme avant, offrig | l’opérateur |
| Définir `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` ou le supprimer dans les propriétés du système | variable supprimée | l’opérateur |
| Écrire l’alias SSH | supprimer le bloc marqué dans `~/.ssh/config` | configuration comme avant | l’opérateur |
| Allouer une voie de projet (la première `offrig_plan` du projet) | supprimer l’entrée du projet de `lanes.toml` une fois qu’aucun pod n’est en cours d’exécution dans sa voie ; un nouveau plan alloue à nouveau | voie libre pour être réutilisée ; le bloc d’alias est séparé (ligne au-dessus) | l’opérateur |
| Écrire le bloc d’alias SSH d’une voie (lancement en tant que processus secondaire) | `offrig_shutdown` le supprime lorsqu’il nomme le pod du plan (également après un lancement ayant échoué) ; sinon, supprimer le bloc `# >>> offrig:offrig-<tag> >>>` dans `~/.ssh/config` | configuration comme avant ; les blocs des autres voies ne sont pas modifiés | l’opérateur |
| Télécharger un modèle sur le pod | `ollama rm <model>` sur le pod, ou terminer le pod | modèle supprimé | l’opérateur |
| Tuer un tunnel offrig orphelin | aucun besoin ; seul un `ssh` avec l’alias exact de cette voie et une redirection est supprimé, jamais celui d’une autre voie | port libre | offrig |
| Lancement en tant que processus secondaire (`offrig_launch`) | `offrig_shutdown` ; automatique si la configuration échoue ; le chien de garde à la date limite ; l’exécuteur lorsque sa file d’attente est vide | pod terminé, plan fermé avec dépenses mesurées | l’agent appelant, le chien de garde servant de sauvegarde |
| Préparer un volume (`offrig stage --yes`, facturation mensuelle) | `offrig stage <profile> --remove --yes` | volume supprimé, profil revenant au téléchargement | la personne qui l’a préparé |
| Démarrer une tâche sur un pod de tâche (`offrig_exec action=start`) | `offrig_exec action=stop` ou `offrig_shutdown` | tâche terminée avec tout ce qu’elle a démarré ; son journal est conservé jusqu’à la suppression du pod | l’agent appelant |
| Exécuter une commande courte sur un pod de tâche (`offrig_exec action=run`) | uniquement ce que la commande elle-même fait ; terminé par `timeout` au plus tard 120 s après le début, ou par `offrig_shutdown` | le pod tel qu’il a été laissé par la commande | l’agent appelant |
| Copier des fichiers vers ou depuis un pod de tâche (`offrig_put`, `offrig_get`) | supprimer la copie (sur le pod, `offrig_exec` ; ici, le fichier) | comme avant la copie | l’agent appelant |

## Disposition

```text
crates/offrig-core   library: RunPod client, pod specs and engine recipes, tunnel, remote
                     ops, Zed and SSH edits, guard, cost and idle logic, session workflow,
                     project lanes, project store, roles, context assembly, checks,
                     runner decisions, watchdog, staging
crates/offrig-cli    `offrig` command line
crates/offrig-app    `offrig-app` desktop app (egui)
crates/offrig-mcp    `offrig-mcp` side-car: MCP server for agents, plus the detached
                     watchdog and runner processes
tools/embed-serve    the CPU embed server for the project index (Python; EmbeddingGemma 2)
docs/                the side-car's design and its research grounding
atlas/               Atlas map of the repo (regenerate with `atlas map`)
```

## Licence

MIT. Voir [LICENSE](LICENSE).

---

Créé par <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>
