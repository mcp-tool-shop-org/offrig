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

Exécutez des modèles volumineux sur des GPU RunPod loués, avec la garantie qu’ils ne seront jamais exécutés sur votre propre GPU. Une application de bureau, une interface en ligne de commande et un module complémentaire pour les agents, le tout basé sur une seule bibliothèque Rust.

Le module complémentaire permet à un agent de planifier une session payante dans le cadre d’un budget défini par l’utilisateur, de louer les GPU et de transmettre une file d’attente de tâches à un exécuteur distinct. L’exécuteur maintient chaque emplacement de modèle occupé, ne révise que les vérifications qui ont échoué et arrête le pod lorsque la file d’attente est vide. Un programme de surveillance termine le pod à la date limite du plan, même si tout le reste est terminé.

## État

Testé avec succès le 2026-10-02 et le 2026-10-03, pour un coût total d’environ 5 $:

- **Frontier :** 4 × RTX PRO 6000 (384 Go) exécutant Qwen3-Coder-480B (AWQ 4 bits) sur SGLang, prêt en 22 minutes, puis 31 transferts effectués en 20 secondes, pour 3,59 $.
- **Données Swarm :** un modèle chargé sert des centaines d’agents simultanément. Le modèle 480B a atteint 4 059 tok/s avec 512 agents ; un modèle 30B sur une seule carte a atteint 10 147 tok/s avec 256 agents.
- **Exécuteur :** une file d’attente avec des dépendances et des renvois de révision, fonctionne sans intervention humaine, le pod s’arrête lorsque la file d’attente est vide.
- **Garantie :** le GPU local est resté inactif pendant toutes les exécutions.

Utilisé quotidiennement depuis le 2026-10-07 par deux projets simultanément, chacun dans son propre environnement : exécutions d’entraînement pour aspire-si sur `job` pods, et rendus de musique pour ai-jam-sessions sur `jam` pods.

Construit et testé, en attente d’une décision : préparation des poids de Frontier sur un volume réseau, environ 21 $/mois (voir [Préparation](#préparation-des-poids-sur-un-volume-réseau)).

Prochainement : la première véritable file d’attente Frontier, planifiée dans son intégralité avant le lancement ; les transferts de code compilés et testés sur le pod.

## Fonctionnement

À partir d’une seule fenêtre (ou d’une seule commande), offrig :

1. affiche le solde de votre compte RunPod, les prix actuels des GPU et la durée pendant laquelle le solde sera suffisant ;
2. lance un pod pour une configuration donnée, d’une petite carte sur Ollama à 4 × RTX PRO 6000 sur SGLang, et charge ses modèles sur le pod ;
3. ouvre un tunnel SSH vers le pod ;
4. ajoute les modèles du pod à Zed en tant que fournisseur distinct ;
5. exécute sept vérifications pour s’assurer que les modèles ne peuvent pas être exécutés sur cette machine ;
6. arrête le pod ou le termine après une période pendant laquelle tous les GPU sont inactifs.

Grâce au module complémentaire, un agent planifie également des sessions en fonction d’un budget, conserve la mémoire du projet entre les compressions et les redémarrages, et exécute des files d’attente de transfert sans surveillance (voir [Le module complémentaire](#le-module-complémentaire-pour-les-agents)).

## La garantie et son fonctionnement

- **Le serveur de modèle est inaccessible sauf via le tunnel.** Le pod exécute un moteur fixe, Ollama (`ollama/ollama:0.35.0`) ou SGLang (`lmsysorg/sglang:v0.5.20-cu130`), lié à sa propre boucle locale, et le pod n’expose que `22/tcp`. Il n’y a pas de point de terminaison HTTP public à trouver ou à exploiter. Une recette ne peut pas déplacer le moteur hors de la boucle locale.
- **Zed communique avec le tunnel, sur son propre port.** Le tunnel écoute sur `127.0.0.1:11435`. Votre Ollama local se trouve sur `11434`. offrig refuse de placer le tunnel sur `11434`, de sorte qu’un tunnel inactif ne peut pas se connecter au serveur local : la requête échoue.
- **Zed ne change jamais de fournisseur.** Les modèles du pod sont un fournisseur `offrig` distinct dans Zed. Si le pod est hors ligne, le fait de choisir l’un d’entre eux génère une erreur ; Zed n’essaie pas un autre fournisseur.
- **Les poids n’existent jamais localement.** Les modèles sont extraits sur le pod, par le pod (ou téléchargés à partir de Hugging Face, ou lus à partir d’un volume réseau préparé).

Les vérifications de sécurité vérifient cela à chaque fois, à partir de faits qu’offrig peut observer :

| Vérification | Échoue lorsque |
|---|---|
| Le tunnel évite le port Ollama local | le port du tunnel est 11434 |
| Zed envoie les modèles du pod via le tunnel | l’URL du fournisseur Zed est autre chose que le tunnel |
| L’Ollama du pod n’est pas exposé à Internet | le pod mappe publiquement le port 11434 |
| Le tunnel se termine sur le pod | la liste des modèles via le tunnel diffère de la liste lue sur le pod via SSH |
| Les modèles du pod ne se trouvent pas sur cette machine | un modèle du pod existe également dans l’Ollama local |
| Aucun modèle du pod ne partage de nom avec un modèle Zed local | un nom dans le fournisseur offrig se trouve également dans la liste Ollama locale de Zed |
| Tous les modèles proposés par Zed se trouvent sur le pod | Zed propose un modèle que le pod n’a pas |

## Installation

Nécessite Windows avec OpenSSH (intégré), Zed si vous souhaitez que les modèles soient disponibles dans un éditeur, et un compte RunPod.

1. Téléchargez `offrig-<version>-windows-x64.zip` à partir de [Releases](https://github.com/mcp-tool-shop-org/offrig/releases), vérifiez-le par rapport au `SHA256SUMS` de la version, et décompressez-le dans votre `PATH`. Il contient `offrig.exe` (l’interface en ligne de commande), `offrig-app.exe` (l’application) et `offrig-mcp.exe` (le module complémentaire). Pour compiler à partir du code source : `cargo build --release`, avec Rust 1.98.1 (version fixée dans `rust-toolchain.toml`).
2. Placez votre clé API RunPod dans la variable d’environnement utilisateur `RUNPOD_API_KEY`.
3. Ajoutez votre clé publique SSH dans les paramètres de compte RunPod. offrig utilise `~/.ssh/runpod_rustline` s’il est présent, puis `~/.ssh/id_ed25519`.
4. Pour les agents, enregistrez le module complémentaire auprès de Claude Code dans l’espace utilisateur : `claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Il ouvre le stockage d’un projet uniquement lors de la première utilisation, il est donc inoffensif dans les projets qui ne l’utilisent jamais.

Le [manuel](https://mcp-tool-shop-org.github.io/offrig/handbook/) explique comment configurer un premier pod, le module complémentaire, la configuration, les environnements et les pods de tâches.

## Utilisation

**Application :** démarrez `offrig-app`, sélectionnez un profil, appuyez sur **Lancer le pod**. Une fois qu’il est prêt, les modèles apparaissent dans le panneau d’agent de Zed sous le nom « RunPod · … ». Redémarrez Zed une fois après le premier lancement pour qu’il reconnaisse `OFFRIG_API_KEY`.

**Interface en ligne de commande :**

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
offrig stage frontier --dc EUR-IS-1 --yes   stage weights on a network volume (bills monthly)
```

### Sortie, codes de sortie et erreurs

- **Niveaux de journalisation :** `-q` affiche uniquement les erreurs et les résultats d’une commande ; `-v` ajoute chaque appel RunPod et son temps d’exécution ; `--debug` ajoute les corps de réponse ayant échoué et les chaînes d’erreurs complètes. La clé API est masquée à chaque niveau.
- **Codes de sortie :** `0` succès, `1` quelque chose à corriger de votre côté (arguments, configuration, refus d’un garde ou d’un budget, clé manquante), `2` une erreur d’exécution (RunPod, réseau, ssh, délai d’attente, capacité insuffisante).
- Les **erreurs du processus secondaire** sont des résultats, jamais des erreurs de protocole : `ok:false` avec un `code` stable, le texte `error`, un `next_action` et un `retryable`. Les codes sont répertoriés dans la [référence du manuel](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## Le processus secondaire (pour les agents)

`offrig-mcp` est un serveur MCP qu’un agent, tel que Claude Code, utilise comme outil. Il conserve une base de données par projet à l’adresse `<project>/.offrig/offrig.db` qui persiste au-delà de chaque pod, de sorte qu’une session survit à une consolidation ou à un redémarrage sans avoir à tout expliquer à nouveau.

| Outil | Fonctionnement |
|---|---|
| `offrig_status` | Le projet, le budget, le solde et la durée de vie de RunPod, les pods d’Offrig, chaque plan ouvert avec son `plan_id`, sa voie et le nom du pod, la file d’attente de transfert avec les tâches obsolètes signalées. |
| `offrig_offers` | Offres GPU en direct pour un nombre de GPU |
| `offrig_plan` | Prix d’une session dans le pire des cas (prix en direct x heures maximales) ; refusé si le budget restant est insuffisant. La réponse indique le `ssh_alias` de la voie et le `pod_name` que le lancement créera, ainsi que le `container_disk_gb` qu’il demandera (l’optionnel `container_disk_gb` remplace le profil ; voir « Disque du conteneur »). Les options `max_price_hr` et `no_fallback` facultatives permettent de limiter les GPU qui peuvent être loués (voir « Affectation du matériel d’un plan ») ; l’option `wait_minutes` facultative définit la durée pendant laquelle le lancement tente de relancer l’opération en cas de capacité insuffisante (voir « Attente de capacité »). |
| `offrig_memory_search` | Recherche la mémoire du projet actif, chaque résultat avec la source et la date |
| `offrig_memory_record` | Ajoute une brève description, une contrainte, une décision, un fait ou un point de contrôle ; les modifications sont des remplacements avec une justification |
| `offrig_handoffs` | Met en file d’attente les transferts dirigés par un rôle (chacun nécessite une vérification d’acceptation ; vérifications déterministes facultatives), les liste, prévisualise les blocs de rôle, affiche la meilleure sortie d’un transfert (également écrite dans `.offrig/out/`), enregistre les résultats (complet, invalide, violation, échec, nouvelle tentative avec retour d’information) |
| `offrig_launch` | **Effectue des dépenses.** N’accepte qu’un `plan_id` : valide le pire des cas, attend que les GPU ne louent rien, démarre le pod, ouvre le tunnel, télécharge les modèles, démarre le chien de garde. Idempotent par plan. Refusé si la voie dispose déjà d’un plan ou d’un pod actif : `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | Progression du lancement (pendant le démarrage du pod, l’étape est dérivée de l’état du pod au moment de l’appel ; chaque nouvelle tentative de capacité est comptabilisée dans `progress.capacity_wait`), le type de GPU et la version CUDA de l’hôte réellement loués (mesurés avec `nvidia-smi` sur le pod, avec une entrée `warnings` bruyante lorsque l’hôte est plus ancien que le seuil CUDA du plan), l’activité du chien de garde, les minutes restantes, les dépenses à ce jour |
| `offrig_ask` | Une itération d’un transfert sur le modèle du pod, le contexte étant construit à partir du stockage du projet ; la réponse est renvoyée en tant que sortie non fiable |
| `offrig_run` | Démarre un exécuteur détaché qui maintient chaque emplacement de modèle occupé : élabore chaque transfert prêt, révise au plus deux fois en cas d’échec des vérifications, transmet les résultats aux transferts dépendants, puis arrête le pod lorsque la file d’attente est vide (sauf si `keep_pod`). Les tâches que le code ne peut pas vérifier sont mises en attente pour examen. |
| `offrig_put` | Copie un fichier ou un répertoire local vers un pod de tâche (scp) ; les chemins de pod relatifs se trouvent sous `/workspace/job`. Optionnel `plan_id` (voir ci-dessous) |
| `offrig_exec` | Exécute une commande bash sur un pod de tâche, de manière détachée afin qu’elle persiste au-delà du processus secondaire (`start`), signale qu’elle est en cours d’exécution ou qu’elle s’est terminée avec son code de sortie et la fin du journal (`status` ; `save_log` copie également l’ensemble du journal dans un fichier local), la tue (`stop`) ou exécute une commande courte maintenant et renvoie sa sortie standard, son erreur standard et son code de sortie (`run`, `timeout_secs` par défaut 30, au maximum 120). Optionnel `plan_id` (voir ci-dessous) |
| `offrig_get` | Copie un fichier ou un répertoire d’un pod de tâche, en créant les dossiers parents locaux manquants ; effectuez cette opération avant l’arrêt, qui supprime le disque du pod. Optionnel `plan_id` (voir ci-dessous) |
| `offrig_shutdown` | **Détruit le pod.** Le termine et clôt les comptes du plan avec les dépenses mesurées ; refusé pendant que les transferts sont en cours, sauf si une justification est fournie. Supprime le bloc `~/.ssh/config` de la voie lorsqu’elle nomme ce pod (`ssh_block_removed`) |

**Sur quel plan de tâche un outil de tâche agit-il.** `offrig_put`, `offrig_exec` et `offrig_get` acceptent un `plan_id` facultatif. S’il n’y a qu’un seul plan de tâche ouvert et aucun `plan_id`, ils l’utilisent, comme auparavant. S’il y a plus d’un plan de tâche ouvert et aucun `plan_id`, ils refusent et répertorient les plans ouverts (ID, profil, nom du pod) : ils ne devinent jamais. Avec un `plan_id`, ils agissent uniquement sur le pod de ce plan, et uniquement après avoir vérifié que le nom du pod est celui que ce plan possède (le pod d’une autre voie est refusé, et non celui d’une autre voie). Chaque réponse de l’outil de tâche, et `offrig_job`, indique le `project` et le `plan_id` sur lesquels il a agi (les réponses de l’outil de tâche indiquent également le `lane`) ; `offrig_status` indique le `project` et répertorie tous les plans ouverts avec leur `plan_id`, leur voie et le nom du pod.

Les rôles proviennent de Role OS (dossiers et cartes de démarrage) ainsi que des quatre rôles de jeu fournis ici dans les formats de Role OS : concepteur de jeu, concepteur de systèmes, concepteur narratif, conservateur de l’histoire. Le plafond du budget est fixé uniquement par un humain :

```text
offrig budget 15          set this project's cap (run in the project directory)
offrig budget             show cap, committed, spent, remaining
```

Chaque lancement démarre un **chien de garde** : un processus distinct qui termine le pod à la date limite du plan (temps validé + heures maximales), même si l’agent, la session ou le processus secondaire ont disparu. Il n’agit jamais sur une recherche ayant échoué, se termine exactement une fois, clôt les comptes et enregistre les données dans `.offrig/watchdog-<plan>.log`. Si la préparation d’un pod loué échoue, le lancement le termine au lieu de le laisser facturer.

La conception et ses preuves se trouvent dans [docs/sidecar-design.md](docs/sidecar-design.md).

### Voies : un processus secondaire par projet, pas de collisions

Deux projets peuvent exécuter des processus secondaires en même temps sur un compte RunPod. Chaque projet dispose de sa propre **voie** : un alias SSH, un port de tunnel et une étiquette de nom de pod que aucun autre projet ne partage.

| | Voie simple (l’interface de ligne de commande, l’application, Zed) | La voie d’un projet |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Port de tunnel | `11435` (exécuteur `11436`) | le premier libre de `11500`, `11502`, ... (exécuteur : le port ci-dessus) |
| Port du processus secondaire (pilote shell) | aucun | `11700` + l’emplacement du canal : `11700`, `11701`, ... |
| Nom du pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Bloc SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` provient du nom du dossier du projet (`aspire-si`, `ai-jam-sessions`), avec un court
hachage ajouté lorsque deux projets partagent le même nom de dossier. L’emplacement d’un canal
est attribué la première fois qu’il planifie une session, enregistré dans le répertoire de configuration
d’offrig (`lanes.toml`) et conservé : le même projet se voit attribuer le même canal après chaque redémarrage.
L’attribution utilise un fichier de verrouillage et écrit le registre de manière atomique, de sorte que
deux processus secondaires démarrés simultanément ne partagent jamais une étiquette, un alias ou un port.
Aucun canal ne peut être `11434` (le port d’Ollama local) : la plage commence à `11500`, et un registre modifié
pour indiquer autre chose est refusé. Un plan enregistre son canal, et son lancement, son exécuteur, son
surveillant et son arrêt utilisent tous ce canal, et non la configuration globale.

Un processus secondaire ne fait qu’associer, lister ou arrêter les pods portant le nom de son propre canal.
L’emplacement d’un autre projet, les pods du canal simple `offrig-<profile>` et tout autre pod du compte sont laissés
intacts : la vérification du lancement d’un pod actif ne demande que son propre canal, l’arrêt refuse
un pod dont le nom n’est pas celui du canal du plan, et la vérification des processus orphelins du tunnel
supprime un processus `ssh` obsolète uniquement lorsque son transfert et son alias sont ceux du canal.
Les plans créés avant l’existence des canaux n’ont aucun canal enregistré et continuent de fonctionner sur
le canal simple, de sorte qu’un pod lancé selon l’ancien schéma est arrêté par le même plan qui l’a
lancé.

**Le propre port du processus secondaire.** `offrig-mcp` communique via MCP sur stdio. Un pilote de shell qui le
maintient ouvert pendant toute une session (lorsque la propre connexion MCP de la session est inactive) le
place derrière un port HTTP en boucle, et ce port était auparavant un numéro unique pour toute la machine
(`11439`) : un deuxième pilote de projet, ou tout autre programme, pouvait le prendre et le premier processus
secondaire s’éteignait sans un mot. La valeur par défaut est maintenant par projet, à partir de l’emplacement
du canal du projet, de la même manière que le port du tunnel : l’emplacement du canal `i` (port du tunnel
`11500 + 2i`) obtient le port du processus secondaire `11700 + i`. La plage `11700` à `11763` se situe au-dessus de chaque port de
tunnel et d’exécuteur qu’un canal peut avoir (`11500` à `11627`), du canal simple `11435` et `11436`, et d’Ollama local
`11434`, de sorte qu’un port de processus secondaire ne puisse jamais être un port de tunnel. Rien de nouveau
n’est stocké : `lanes.toml` n’est pas modifié et le port découle du canal. `OFFRIG_SIDECAR_PORT` continue de le remplacer ; une
valeur qui n’est pas un port, qui est inférieure à 1024, ou qui est `11434`, `11435`, `11436` ou toute autre valeur
dans la plage du tunnel du canal est refusée.

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Avec `--check`, un port pris est une erreur qui indique le port et, lorsqu’un processus secondaire offrig y
répond, le projet qu’il prend en charge : « le port du processus secondaire 11700 est pris : un processus
secondaire offrig prend déjà en charge le projet <chemin> ici. Arrêtez-le d’abord, ou définissez
OFFRIG_SIDECAR_PORT sur un port libre. » La vérification demande de la même manière que le pilote y
répond déjà (une requête en tant que projet que personne ne prend en charge, ce que le pilote refuse avant
de toucher à un outil), de sorte qu’elle ne change rien dans un processus secondaire en cours d’exécution.
`offrig_status` signale l’emplacement du canal `sidecar_port`.

**Un canal, un seul pod actif.** Un canal dispose d’un seul alias SSH et d’un seul port de tunnel, de sorte
qu’il prend en charge un seul pod à la fois : un deuxième pod dans le canal (`offrig-<tag>-job` à côté de `offrig-<tag>-jam`) réaffecterait
l’alias à lui-même et enverrait le `offrig_put`, `offrig_exec` et `offrig_get` du premier plan à la mauvaise machine. `offrig_launch` refuse donc
tant que le canal dispose d’un plan ouvert ou de tout pod actif qu’il possède, avec « le canal <étiquette>
dispose d’un pod actif <nom> (plan <id>) ; arrêtez-le d’abord`, before anything is committed or rented. The plain lane's `offrig up » et l’application refuse de la
même manière pour un pod d’un autre profil (le pod du même profil est toujours réutilisé). Arrêtez le
premier plan, puis lancez le suivant.

## Niveaux

Les profils sont stockés dans `%APPDATA%\offrig\config.toml` (écrits lors du premier changement). Valeurs par défaut :

| Profil | GPU | Modèles | Coût typique |
|---|---|---|---|
| petit | 1 × RTX 2000 Ada / A4000 | `qwen3:4b` | environ 0,25 $/heure |
| moyen | 1 × RTX PRO 6000 (96 Go) ; A100 ou H100 80 Go s’il n’y en a pas de libre | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | 2,09 $/heure (A100 en secours : 1,59 $) |
| avant-garde | 4 × RTX PRO 6000 (384 Go), **SGLang** | Qwen3-Coder-480B AWQ 4 bits (252 Go), environ 130 Go restants pour le contexte | 8,36 $/heure |
| avant-garde-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 Go) : le chemin du moteur d’avant-garde, répété à moindre coût | environ 1,7 $/heure |
| avant-garde-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 Go) : les noyaux MoE 4 bits de l’avant-garde, répétés à moindre coût | environ 1,7 $/heure |
| tâche | 1 × RTX PRO 6000 (96 Go) ; A100 ou H100 80 Go s’il n’y en a pas de libre | aucun : un **pod de tâche** exécute votre travail, et non un serveur de modèle | 2,09 $/heure (A100 en secours : 1,59 $) |
| jam | 1 × A40 (48 Go) en premier ; A6000, A5000, 3090, L4 ou 4090 s’il n’y en a pas de libre | aucun : un **pod de tâche** pour les rendus de chant d’ai-jam-sessions (SoulX-Singer) | 0,49 $/heure (A40) |

Un profil avec un `recipe` exécute un autre moteur qu’Ollama : une image épinglée (`lmsysorg/sglang:v0.5.20-cu130`), un modèle Hugging Face
qu’il télécharge au démarrage, et des arguments de serveur supplémentaires. offrig définit le parallélisme
des tenseurs à partir du nombre de GPU, la longueur du contexte à partir du profil, et conserve le moteur
sur la boucle de rétroaction du pod ; une recette ne peut pas remplacer ces paramètres. Pour un dépôt
protégé, `hf_token_secret` indique un secret RunPod, référencé en tant que `{{ RUNPOD_SECRET_<name> }}` afin que le jeton n’entre jamais dans
les spécifications du pod. Le lancement attend le `/health` et la liste des modèles du moteur, signale les
poids sur le disque pendant qu’il les télécharge, et s’arrête immédiatement (avec le journal du moteur)
si le moteur se termine.

Chaque profil répertorie les types de GPU par ordre de priorité ; RunPod prend le premier avec une capacité
suffisante. Lorsqu’aucun n’est disponible, un profil peut attendre (`wait_for_gpu_minutes` ; l’avant-garde attend jusqu’à 120
minutes) : offrig vérifie chaque minute et crée le pod dès que les GPU se libèrent. Rien n’est loué
pendant qu’il attend, Ctrl+C ou l’annulation du lancement de l’application l’arrête, et si l’API de prix
de RunPod est hors service, il réessaie simplement de créer le pod chaque minute. Les grandes configurations
multi-GPU arrivent et disparaissent en quelques minutes. Les prix sont les prix secure-cloud, lus en direct ;
la page de tarification n’est pas le prix disponible.

### En attente de capacité

Un plan dont la capacité est réduite avec `no_fallback` ou `max_price_hr` échoue souvent, il réessaie donc discrètement au lieu d’échouer, sans rien louer pendant ce temps. La durée d’attente est, par ordre : le `wait_minutes` du plan (un argument `offrig_plan`, stocké avec le plan ; `0` échoue immédiatement), sinon le `wait_for_gpu_minutes` du profil. Le profil `job` a une valeur par défaut de 20 minutes. L’attente est réduite au temps restant pour le plan, moins une réserve de cinq minutes, de sorte qu’il ne dépasse jamais la date limite du plan, et comme rien n’est loué pendant l’attente, cela n’ajoute rien au pire scénario prévu. `offrig_launch` signale `capacity_wait_minutes` ; pendant l’attente, `offrig_job` affiche `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) et une étape qui indique de quelle vérification il s’agit. Lorsque l’attente est terminée, le lancement échoue avec `no capacity` et rien n’a été loué.

### Définir la configuration matérielle d’un plan

Un profil répertorie les types de GPU par ordre de priorité, et RunPod prend le premier qui a de la capacité disponible. Ainsi, sans limites, un plan peut se retrouver avec une carte de secours dotée de moins de mémoire, d’un pilote plus ancien et d’un prix différent. Trois limites permettent de restreindre un plan au matériel qu’il peut utiliser. La planification reste gratuite ; les limites ne font que restreindre ce que le plan peut louer.

| Limite | Où | Effet |
|---|---|---|
| `min_cuda` | profil (`config.toml`) | La version CUDA la plus ancienne du hôte, provenant de la liste de RunPod (`13.0`, `12.9`, ... `11.8`). La création du pod envoie chaque version égale ou supérieure en tant que `allowedCudaVersions`. Pour un profil de tâche, la version la plus récente parmi celle-ci et la version `[profiles.job] min_cuda` de l’image est appliquée. |
| `min_vram_gb` | profil | La quantité minimale totale de VRAM (toutes les GPU du profil combinées) qu’un plan accepte. Les offres inférieures à cette valeur sont rejetées ; un type pour lequel RunPod ne répertorie aucune mémoire est également rejeté. |
| `max_price_hr` | argument `offrig_plan` | Le coût maximal du pod, en $/h au total pour toutes ses GPU (le chiffre affiché par `offrig_offers`). Les offres supérieures à cette valeur sont rejetées, ainsi qu’un type pour lequel aucun prix n’est actuellement indiqué (il ne peut pas être soumis à une limite). |
| `no_fallback` | argument `offrig_plan` | Seule la première famille de GPU du profil est autorisée. Les deux éditions RTX PRO 6000 Blackwell (Serveur et Station de travail) constituent une seule famille ; toutes les autres cartes, y compris les A100 SXM et PCIe, constituent leur propre famille. |

Les deux champs du profil sont facultatifs et ont par défaut une valeur non définie, de sorte qu’un `config.toml` écrit par un offrig précédent est chargé sans modification. Le profil `job` définit `min_cuda = "13.0"`.

`offrig_plan` évalue le pire des cas en fonction de ce qui reste :
`max_hours x min(max_price_hr, the dearest listed price among the remaining GPUs)`. Sans
`max_price_hr`, il s’agit du prix le plus élevé répertorié dans le profil, comme auparavant. Un plan pour lequel il ne reste plus rien est refusé, avec la raison de chaque GPU rejeté, et rien n’est écrit.

Le plan stocke la liste des GPU qui restent et la valeur minimale de CUDA, et `offrig_launch` ne loue que parmi ceux-ci, jamais à partir de la liste complète du profil. `offrig_job` et le résultat du lancement du processus secondaire signalent le type de GPU et la version CUDA du hôte qui ont été réellement loués, dans un objet `rented`. L’API du pod ne signale pas la version CUDA du hôte, donc une fois que ssh est opérationnel, le lancement exécute `nvidia-smi` une seule fois et la lit dans l’en-tête (`CUDA Version: 12.8`, ou `CUDA UMD Version: 13.4` pour les pilotes plus récents) ; `rented.cuda_source` indique `nvidia-smi` ou `pod API`. Si la version CUDA du hôte est antérieure à la valeur minimale du plan, ou si le GPU ne figure pas dans la liste du plan, ou si le prix est supérieur à celui du plan, `offrig_job` renvoie une entrée `warnings` et démarre `next_action` avec `WARNING`. Rien n’est arrêté automatiquement : l’arrêt de la location dépend de l’appelant (`offrig_shutdown`). Si ni l’API du pod ni `nvidia-smi` ne fournissent une version CUDA, `rented.notes` l’indique et la valeur minimale n’est pas vérifiée.

### Disque du conteneur

Un pod dispose de deux disques : le disque du conteneur, qui est local au hôte, et le volume monté à `/workspace`. Sur certains hôtes, `/workspace` est un système de fichiers réseau lent : un pod de tâche a mesuré 32 Mo/s, contre 354 Mo/s sur son disque de conteneur, et n’a pas pu récupérer environ 130 Go de modèles à temps, alors que le disque du conteneur n’était que de 60 Go. La taille du disque du conteneur est la valeur `container_disk_gb` du profil (de 30 à 60 Go dans les profils intégrés ; le profil `job` a une taille de 60 Go) et est transmise à la création du pod en tant que `containerDiskInGb`. `offrig_plan` prend `container_disk_gb` (de 1 à 2000) pour la remplacer pour un plan ; le plan la stocke, le lancement la transmet, et le plan de réponse et `offrig_status` affichent la taille en vigueur.

- Le disque du conteneur n’est pas facturé : offrig facture uniquement le temps d’utilisation du GPU, de sorte que le pire des cas du plan est le même quelle que soit la taille. RunPod facture le disque ; il n’a pas été vérifié ici si le tarif qu’il indique pour le pod (`offrig_job` l’affiche) comprend le disque du conteneur.
- offrig ne déplace pas vos téléchargements. Les commandes de tâche commencent par `HF_HOME` sur le volume `/workspace` (`/workspace/hf`) ; pour utiliser le disque du conteneur, définissez le vôtre (`HF_HOME=/root/hf python ...`) dans la commande.
- Le disque du conteneur est supprimé avec le pod, comme le volume sans volume réseau : copiez les résultats avec `offrig_get` avant `offrig_shutdown`.
- La limite de 1 à 2000 est une vérification de cohérence propre à offrig contre une erreur de frappe ; la limite réelle de RunPod n’est pas vérifiée.

### Pods de tâche

Un profil avec un `job` loue un GPU pour un travail qui s’exécute dessus, comme une exécution d’entraînement, plutôt que pour servir un modèle. Son pod exécute une image PyTorch définie (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 pour Blackwell) avec sshd et rien d’autre.

- Il ne prend en charge aucun modèle, il n’y a donc pas de tunnel et rien n’est connecté à Zed. sshd n’autorise aucun
redirection (`AllowTcpForwarding=no`) ; la seule façon d’y accéder est via ssh vers le pod.
- Un profil de tâche ne répertorie aucun modèle et ne peut pas non plus avoir de recette ; la vérification de la configuration refuse
les deux.
- `offrig up` et l’application refusent un profil de tâche avant de louer quoi que ce soit. Il s’exécute via
le conteneur auxiliaire : `offrig_plan profile=job`, `offrig_launch`, puis `offrig_put`,
`offrig_exec` et `offrig_get`. Le lancement est prêt lorsque sshd répond.
- Une commande s’exécute de manière indépendante sur le pod (`setsid nohup`) dans `/workspace/job`, de sorte qu’elle dépasse
la durée de vie du conteneur auxiliaire et de la session ssh. Elle est envoyée au format base64, de sorte que rien de ce qu’elle contient n’est lu par
le shell ssh. Son journal et son statut de sortie sont conservés dans `/workspace/offrig/jobs/`. Les téléchargements de Hugging Face
sont effectués dans `/workspace/hf` sur le volume du pod.
- `offrig_exec action=run` est destiné aux vérifications rapides (`ls`, `nvidia-smi`), et non au travail : il exécute
la commande jusqu’à son terme sous `timeout` (par défaut 30 s, maximum 120 s) et renvoie `stdout`,
`stderr`, `exit_code` et `timed_out`. La sortie est tronquée aux 64 Ko derniers de chaque flux
(`truncated`) et il s’agit d’une sortie de pod non fiable. Une commande qui nécessite plus de temps est une `start`.
- La fin du journal d’une tâche affiche des barres de progression réduites : les mises à jour de style tqdm, jointes par des retours chariot, n’affichent que leur dernière image. `offrig_exec action=status save_log=<local path>`
copie également l’ensemble du journal de la tâche, tel qu’il a été écrit, dans un fichier local (les dossiers parents sont créés),
de sorte que la fin du journal puisse rester courte.
- L’image est une version CUDA 12.8, un profil de tâche indique donc la version CUDA la plus ancienne de l’hôte sur laquelle elle
s’exécute (`min_cuda = "12.8"`) et le pod est créé à partir de celle-ci avec `allowedCudaVersions` de RunPod. Sans cela, un hôte doté d’un pilote plus ancien démarre le pod et torch ne trouve pas de GPU,
après le début de la location. Le travail lui-même peut nécessiter plus que l’image : le profil `job`
définit également `min_cuda = "13.0"` sur le profil (voir ci-dessus), car les tâches qu’il exécute installent une
version actuelle de vLLM, dont PyTorch est une version CUDA 13.
- Le budget, le plan, la surveillance et l’arrêt fonctionnent comme pour tout autre profil. Copiez les résultats avant `offrig_shutdown` : le disque du pod est inclus.
- `jam` est le profil de tâche que ai-jam-sessions utilise pour rendre ses chants : SoulX-Singer nécessite beaucoup
moins qu’une carte d’entraînement, il loue donc une carte bon marché de 24 à 48 Go. La configuration et la session
se trouvent dans ce dépôt (`docs/vocal-offrig.md`) ; offrig ne sait rien sur le chant.

### Mise en place des poids sur un volume réseau

Un profil de recette télécharge ses poids à chaque lancement : pour le modèle de pointe, cela prenait environ
20 minutes sur 22 pour être prêt (252 Go, 8,36 $/h). La mise en place les place sur un volume réseau RunPod une seule fois :

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- Le volume est facturé mensuellement, qu’un pod s’exécute ou non (300 Go pour le modèle de pointe représentent environ
21 $/mois à 0,07 $/Go), de sorte que seule une personne effectue la mise en place ; aucun outil d’agent ne peut le faire.
- Un volume se trouve dans un seul centre de données, de sorte que les pods du profil ne sont lancés que là-bas, et
les offres et les plans sont tarifés là-bas. Choisissez-en un doté d’un stockage réseau et des GPU du profil ; `offrig gpus` et la console RunPod indiquent où ils se trouvent.
- Le téléchargement s’effectue sur le pod GPU le moins cher disponible dans ce centre de données. Le pod est
terminé en cas de succès, d’échec ou de dépassement du délai d’attente.
- Le volume est enregistré dans le profil avant le début du téléchargement, de sorte qu’une mise en place ayant échoué n’est jamais oubliée ; relancez-la pour reprendre, ou `--remove`.
- Un lancement mis en place exécute Hugging Face hors ligne, uniquement lorsque la mise en place est terminée (un marqueur sur
le volume). Un volume partiellement mis en place télécharge le reste au lieu d’échouer.

## Sécurité financière

- Avant un lancement, offrig affiche la correspondance gratuite la moins chère et votre marge de manœuvre avec le pod
en cours d’exécution. Si la marge de manœuvre est inférieure à une heure, il refuse, sauf si vous la modifiez, car à zéro,
RunPod arrête tous les pods du compte, y compris ceux qu’offrig ne gère pas.
- L’arrêt automatique termine le pod après 30 minutes si chaque GPU est utilisé à moins de 5 % (configurable,
ou désactivé).
- La fermeture de l’application avec un pod en cours d’exécution demande si vous souhaitez le terminer ou le laisser en cours d’exécution.
- offrig ne touche que les pods qu’il a nommés : `offrig-<profile>` pour l’interface de ligne de commande et l’application,
`offrig-<tag>-<profile>` pour la voie auxiliaire d’un projet (voir Voies). Un conteneur auxiliaire ne touche jamais
les pods d’une autre voie, les pods de la voie principale ou tout autre pod ; ceux-ci sont répertoriés,
mais jamais modifiés.
- Pour les sessions d’agent, la limite est appliquée avant toute dépense : le pire des cas d’un plan (prix en direct × heures maximales) est pris en compte par rapport au budget défini par l’utilisateur et refusé s’il est dépassé, et
un lancement ne prend qu’un ID de plan, de sorte qu’un agent ne peut pas fixer son propre prix.
- Chaque lancement de conteneur auxiliaire dispose d’une surveillance qui termine le pod à la date limite du plan,
et l’exécuteur arrête le pod dès que sa file d’attente est vide.

## Ce que cela modifie sur votre machine

| Quoi | Où | Annuler |
|---|---|---|
| Fournisseur Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove` ; le premier original est conservé en tant que `settings.json.offrig.bak` |
| Modèle par défaut de Zed (uniquement si vous le demandez) | même fichier | `offrig zed-remove` restaure le modèle par défaut précédent |
| `OFFRIG_API_KEY` (espace réservé ; Zed souhaite une clé) | environnement utilisateur | `setx OFFRIG_API_KEY ""` ou supprimez-le dans les propriétés du système |
| Alias SSH `offrig` | `~/.ssh/config`, entre les marqueurs `# >>> offrig:offrig >>>` | supprimez le bloc marqué |
| Alias SSH `offrig-<tag>`, un par projet lancé à partir d’un conteneur auxiliaire | `~/.ssh/config`, entre les marqueurs `# >>> offrig:offrig-<tag> >>>` | supprimez le bloc marqué |
| Voies de projet | `%APPDATA%\offrig\lanes.toml` (chemin du projet, balise, alias, port de tunnel) | supprimez l’entrée du projet lorsqu’aucun pod ne s’exécute dans sa voie, ou supprimez l’ensemble du fichier |
| Clés d’hôte de pod | `~/.ssh/known_hosts_offrig` | supprimez le fichier |
| Paramètres | `%APPDATA%\offrig\config.toml` | supprimez le fichier |
| Poids mis en place (uniquement avec `offrig stage --yes`) | un volume réseau RunPod `offrig-<profile>` ; facturé mensuellement | `offrig stage <profile> --remove --yes` |

Les commentaires et la mise en page dans les paramètres de Zed sont conservés : les modifications sont effectuées via un arbre syntaxique JSONC.

## Modèle de menace

- **Clé API RunPod.** Lue à partir de `RUNPOD_API_KEY` ; jamais écrite sur le disque ou dans les journaux. Le fournisseur de Zed ne s’appelle volontairement pas `runpod` : sous ce nom, Zed lirait `RUNPOD_API_KEY` et l’enverrait au serveur de modèle.
- **Serveur de modèle.** Accessible uniquement via SSH avec votre clé. L’authentification par mot de passe est désactivée sur le pod, et sshd n’autorise que le transfert local.
- **Clés d’hôte.** Enregistrées pour chaque point de terminaison dans un fichier known-hosts distinct. offrig oublie une clé uniquement lorsque le point de terminaison du pod change, car RunPod réutilise les paires adresse IP:port entre les pods.
- **Injection de code.** Les noms des modèles sont validés par rapport à la syntaxe des noms d’Ollama avant d’atteindre un shell distant.
- **Tunnels orphelins.** Si offrig se termine, son `ssh` peut continuer à maintenir le port. Au prochain démarrage, offrig le termine, mais uniquement si l’écouteur transporte `ssh.exe`, qui correspond exactement aux spécifications de transfert d’offrig. Tout autre processus utilisant le port est refusé et n’est jamais terminé.
- **Pas de télémétrie.** offrig communique uniquement avec l’API de RunPod, votre pod et votre instance locale d’Ollama (pour comparer les listes de modèles).

## Tests

`cargo test --workspace` exécute plus de 250 tests, couvrant au moins 90 % des lignes (les tests CI échouent en dessous de ce seuil) :

- **La bibliothèque principale :** analyse de RunPod, spécifications des pods pour les deux moteurs, configuration SSH, modifications JSONC de Zed, règles de protection, logique de coût et d’inactivité, le stockage et ses migrations, rôles, assemblage du contexte, vérifications déterministes, décisions du moteur, le système de surveillance et la mise en scène, y compris un RunPod simulé qui prouve qu’une étape ayant échoué termine son pod.
- **L’application :** gestion de l’état et tests d’interface utilisateur interactifs dans l’environnement de test d’egui.
- **L’interface de ligne de commande :** codes de sortie, niveaux de journalisation et le fait que la clé API n’apparaît jamais dans la sortie.
- **Le processus secondaire :** tests de bout en bout via stdio avec un RunPod simulé, le processus de surveillance réel et le processus de moteur réel avec un modèle de pod simulé ; chaque erreur d’outil est associée à un code.

`scripts/verify.sh` (ou `scripts/verify.ps1`) exécute la vérification du format, clippy, les tests et une exécution de test de chaque binaire dans une seule commande. Les tests CI exécutent également `cargo deny`, une analyse OSV de `Cargo.lock`, la couverture vers Codecov et `atlas check`.

### Enregistrement des tests en direct (2026-10-02, niveau moyen, A100 80 Go, environ 0,45 $)

- Le pod est opérationnel en environ 80 s ; sshd, le tunnel et l’instance Ollama 0.35.0 du pod répondent.
- 97 Go de modèles sont téléchargés sur le pod à un débit d’environ 150 à 250 Mo/s.
- `qwen3-coder:30b-a3b-q8_0` et `gpt-oss:120b` ont chacun répondu à une conversation en streaming avec un appel d’outil correct via le tunnel. Ils ont utilisé 36 Go et 64 Go de la VRAM du pod ; l’instance Ollama locale n’a rien chargé et n’était pas du tout présente sur le GPU local.
- Les sept vérifications de protection ont été réussies, à partir de l’interface de ligne de commande et de l’application.
- Une interface de ligne de commande arrêtée de manière abrupte a laissé son `ssh` maintenir le port ; la prochaine exécution l’a récupéré.
- Le tunnel, les vérifications, le test de modèle et l’arrêt de l’application ont été lancés via ses boutons.

Bogues détectés lors de l’exécution en direct, maintenant corrigés et couverts : une liste `&&` en arrière-plan a maintenu stdout de ssh ouvert et a bloqué le démarrage du téléchargement ; la liste des pods ne contenait pas les types de GPU sans `includeMachine=true` ; la vérification de lancement a compté deux fois le prix d’un pod en cours d’exécution.

### Répétition du processus secondaire (2026-10-03, niveau faible, RTX 2000 Ada, 0,08 $ réservé)

L’instance `offrig-mcp` installée est exécutée via stdio, comme un agent le ferait :

- `offrig_plan` a estimé 0,5 h à 0,15 $ dans le pire des cas ; `offrig_launch` l’a validé, a démarré le système de surveillance, et un deuxième appel a renvoyé la même tâche. Un pod a été loué, à 0,24 $/h.
- SSH est opérationnel 100 s après la location, `qwen3:4b` a été téléchargé, prêt en 150 s.
- `offrig_ask` a exécuté un transfert de tâches de concepteur de jeu en 46 s ; la réponse a satisfait sa vérification d’acceptation et a conservé la contrainte de cinq éléments de la mémoire.
- Le pod a accédé à Internet (Wikipedia, API GitHub). Sur la base des données récupérées par le pod, le modèle a répondu correctement aux questions actuelles ; lorsqu’on lui a posé des questions à froid, il a déclaré qu’il n’avait pas d’accès en direct.
- `offrig_shutdown` à partir d’un processus secondaire fraîchement démarré a terminé le pod et a clôturé les comptes ; le système de surveillance a vu la fin du plan et s’est arrêté. Le GPU local est resté inactif tout au long du processus.

Détecté et corrigé : le pod a traité une requête à la fois (`OLLAMA_NUM_PARALLEL=1`) ; quatre emplacements ont traité 8 requêtes parallèles provenant de 40 à 102 tokens/s sur le même GPU, de sorte que chaque profil dispose désormais de `parallel = 4`. `complete` a été refusé sans raison (il a maintenant une valeur par défaut de « vérification d’acceptation réussie » ; les échecs doivent toujours en avoir une). L’état indiquait qu’il fallait enregistrer un bref instant pendant qu’une session était active. On suppose que le texte qui s’infiltre dans une réponse est supprimé, et qu’une réponse vidée par la réflexion indique qu’il faut augmenter `max_tokens`.

### Répétition du moteur (2026-10-03, niveau faible, RTX 2000 Ada, 0,04 $ réservé)

Cinq transferts de tâches, dont un dépendant de l’autre, ont fonctionné avec `offrig_run` sans qu’aucun moteur ne les contrôle :

- Quatre transferts de tâches en cours simultanément sur quatre emplacements (12,9 Go sur 16 Go de VRAM) ; le transfert de tâches dépendant a commencé dès que sa dépendance a été terminée et a été basé sur son résultat.
- Les trois transferts de tâches dont les vérifications couvraient l’acceptation ont été terminés par eux-mêmes ; les histoires rivales (vérifications partielles) et le récit (sans vérifications) ont été envoyés pour examen.
- L’examen a renvoyé le récit (« la rivière porte le nom du projet ») ; le moteur en direct l’a adopté et l’a révisé en fonction des commentaires (« rivière Veyl »).
- La file d’attente s’est vidée en 6,5 minutes (6 tours, 20 861 tokens) ; le moteur a arrêté le pod lui-même.

Appris : les vérifications déterministes vérifient la structure, et non la qualité de la conception. Le modèle 4 B a réussi « trois verbes » avec des verbes peu précis, de sorte que `accept_on_checks` est destiné aux travaux structurels et que les travaux de conception sont envoyés pour examen. qwen3 :4b a passé environ 4 000 tokens à réfléchir par tour, même sur trois lignes de récit. Une file d’attente maintient chaque emplacement occupé uniquement lorsqu’elle contient suffisamment de transferts de tâches indépendants ; une chaîne de dépendances s’exécute un par un.

### Exécutions Frontier et SGLang (2026-10-03, 4,27 $ réservé)

| Exécuter | Pod | Prêt après | File d’attente | Réservé |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 min | 4 transferts de tâches en 15 s | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 min | 4 transferts de tâches | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 min** (252 Go à 278 Mo/s, puis chargement) | **31 transferts en 20 s** | **$3.59** |
| Balayage de 30 milliards de paramètres | 1 × RTX PRO 6000 | 10 min (placement lent des pods) | balayage uniquement | $0.43 |

- SGLang v0.5.20 (cu130) s’exécute sur Blackwell : attention flashinfer, `awq_marlin` pour les
poids MoE à 4 bits, parallélisme tensoriel sur PCIe sur quatre cartes ; `/dev/shm` était de 352 Go.
- Le travail de la version la plus récente était clairement meilleur que celui des petits modèles : dialogues dans le monde virtuel et un
module Rust qui a été compilé et a réussi ses trois tests (vérifiés localement). La version de la même tâche avec 30 milliards de paramètres FP8 n’a pas été compilée.
- Un modèle chargé sert l’ensemble d’un essaim ; aucun exemplaire n’est nécessaire. Balayage de concurrence avec
des réponses de 384 jetons, nombre total de jetons par seconde :

| Agents | 480 milliards de paramètres sur 4 GPU | 30 milliards de paramètres AWQ sur 1 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

La vitesse par agent diminue à mesure que des agents sont ajoutés (480 milliards : 88 → 24 jetons/s à 64), mais le débit total continue d’augmenter ; les gains de 480 milliards de paramètres se stabilisent après 256. La mémoire cache KV de la version la plus récente contient 398 526 jetons, de sorte qu’avec de véritables contextes de transfert de 2 à 8 000 jetons, la version la plus récente exécute désormais 64 transferts simultanément et les versions SGLang à une seule carte 32.

Ce qui a été trouvé et corrigé en cours de route : les révisions ont ajouté des éléments de sortie pour réussir une vérification d’en-tête (maintenant une
vérification `no_repeats` intégrée, et les révisions restructurent sur place) ; la sortie requise d’un rôle a été divulguée dans les livrables (le transfert définit désormais le format) ; les commentaires d’en-tête nomment le format Markdown ; une révision inchangée s’arrête au lieu de se répéter.

Pas encore vérifié en direct : un chat
envoyé directement à partir du panneau d’agent de Zed (la forme de la requête utilisée par Zed est testée directement).

## Conformité aux normes

Évaluation par rapport aux normes de flux de travail du studio (0 manquantes, 1 partielle, 2 présentes,
3 exemplaires).

- **PIN_PER_STEP : 2.** Les images des pods sont fixées aux étiquettes de version (`ollama/ollama:0.35.0`,
`lmsysorg/sglang:v0.5.20-cu130` ; une recette refuse `latest`), le compilateur à 1.98.1,
les dépendances par `Cargo.lock` et le moteur Atlas à la version 1.24.0 du parc. Chaque transfert
enregistre son modèle, son hachage de rôle et son hachage d’invite. Les modèles sont fixés par étiquette ou par ID de dépôt,
et non par hachage.
- **ANDON_AUTHORITY : 3.** Chaque étape interrompt l’exécution en cas de défaut : un plan dont les poids
dépassent la capacité du disque est refusé avant toute dépense ; un échec de téléchargement arrête le lancement ; une modification Zed
qui ne peut pas être relue n’est pas écrite ; un fichier de paramètres défectueux est signalé, mais jamais
réécrit ; CI bloque sur fmt, clippy, tests, licences et avis.
- **NAMED_COMPENSATORS : 2.** Chaque action irréversible a une annulation, répertoriée ci-dessous.
- **DECOMPOSE_BY_SECRETS : 2.** Un module par élément qui change pour ses propres raisons :
l’API de RunPod (`runpod`), le contenu du pod (`spec`), le transport (`tunnel`,
`remote`), chaque fichier local modifié par offrig (`sshconfig`, `zed`) et les règles (`guard`,
`cost`). Les interfaces ne contiennent aucune logique au-delà de la présentation.
- **UNCERTAINTY_GATED_HUMANS : 2.** offrig ne pose de questions que lorsque le résultat est coûteux ou
entraînant une perte : lancement avec moins d’une heure de temps disponible, arrêt d’un pod (en indiquant ce qui est perdu) et
arrêt avec un pod qui continue de facturer. Deux décisions appartiennent uniquement à un humain, et aucun
outil d’agent ne peut les prendre : le plafond budgétaire et la préparation d’un volume, qui est facturé mensuellement.
La sortie du transfert que le code ne peut pas vérifier est mise en attente d’examen au lieu d’être terminée.
- **EXTERNAL_VERIFIER : n/a.** Aucune revendication spécialisée.

**Compensateurs**

| Action | Annuler | État après annulation | Propriétaire |
|---|---|---|---|
| Créer un pod (début de la facturation) | `offrig down <profile> --yes`, l’application s’arrête ou s’arrête automatiquement | pod terminé, facturation arrêtée | l’opérateur exécutant offrig |
| Terminer un pod | aucun pour son disque ; relancer le profil et les modèles sont retéléchargés (un volume réseau les conserve) | nouveau pod, même profil | l’opérateur |
| Écrire le fournisseur Zed ou le modèle par défaut | `offrig zed-remove`, ou restaurer `settings.json.offrig.bak` | Zed comme avant offrig | l’opérateur |
| Définir `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` ou le supprimer dans les propriétés du système | variable supprimée | l’opérateur |
| Écrire l’alias SSH | supprimer le bloc marqué dans `~/.ssh/config` | configuration comme avant | l’opérateur |
| Allouer une voie de projet (la première `offrig_plan` du projet) | supprimer l’entrée du projet de `lanes.toml` une fois qu’aucun pod ne s’exécute dans sa voie ; un nouveau plan alloue à nouveau | voie libre pour réutilisation ; le bloc d’alias est séparé (ligne ci-dessus) | l’opérateur |
| Écrire le bloc d’alias SSH d’une voie (lancement en tant que processus secondaire) | `offrig_shutdown` le supprime lorsqu’il nomme le pod du plan (également après un lancement ayant échoué) ; sinon, supprimer le bloc `# >>> offrig:offrig-<tag> >>>` dans `~/.ssh/config` | configuration comme avant ; les blocs des autres voies ne sont pas affectés | l’opérateur |
| Télécharger un modèle sur le pod | `ollama rm <model>` sur le pod, ou terminer le pod | modèle supprimé | l’opérateur |
| Tuer un tunnel offrig orphelin | rien n’est nécessaire ; seul un `ssh` avec l’alias et le transfert exacts de cette voie est tué, jamais celui d’une autre voie | port libre | offrig |
| Lancement en tant que processus secondaire (`offrig_launch`) | `offrig_shutdown` ; automatique si la configuration échoue ; le chien de garde à la date limite ; l’exécuteur lorsque sa file d’attente est vide | pod terminé, plan clôturé avec les dépenses mesurées | l’agent appelant, le chien de garde servant de sauvegarde |
| Préparer un volume (`offrig stage --yes`, facturation mensuelle) | `offrig stage <profile> --remove --yes` | volume supprimé, profil revenant au téléchargement | l’humain qui l’a préparé |
| Démarrer un travail sur un pod de travail (`offrig_exec action=start`) | `offrig_exec action=stop`, ou `offrig_shutdown` | travail terminé avec tout ce qu’il a démarré ; son journal reste jusqu’à la suppression du pod | l’agent appelant |
| Exécuter une commande courte sur un pod de travail (`offrig_exec action=run`) | uniquement ce que la commande elle-même fait ; terminé par `timeout` au plus tard après 120 s, ou par `offrig_shutdown` | le pod tel qu’il a été laissé par la commande | l’agent appelant |
| Copiez des fichiers vers ou depuis un pod de tâches (`offrig_put`, `offrig_get`) | supprimez la copie (sur le pod, `offrig_exec` ; ici, le fichier) | comme avant la copie | l’agent appelant |

## Mise en page

```text
crates/offrig-core   library: RunPod client, pod specs and engine recipes, tunnel, remote
                     ops, Zed and SSH edits, guard, cost and idle logic, session workflow,
                     project lanes, project store, roles, context assembly, checks,
                     runner decisions, watchdog, staging
crates/offrig-cli    `offrig` command line
crates/offrig-app    `offrig-app` desktop app (egui)
crates/offrig-mcp    `offrig-mcp` side-car: MCP server for agents, plus the detached
                     watchdog and runner processes
docs/                the side-car's design and its research grounding
atlas/               Atlas map of the repo (regenerate with `atlas map`)
```

## Licence

MIT. Voir [LICENSE](LICENSE).

---

Créé par <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>
