<p align="center">
  <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.hi.md">हिन्दी</a> | <a href="README.md">English</a> | <a href="README.pt-BR.md">Português (BR)</a>
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

[![M8ven Score](https://m8ven.ai/badge/mcp/mcp-tool-shop-org-offrig-790exm?v=3bd60e98b65d7124b74c4126c5ce5ce7)](https://m8ven.ai/mcp/mcp-tool-shop-org-offrig-790exm?s=readme)

Esegui modelli di grandi dimensioni su GPU RunPod noleggiate, con una garanzia: non verranno mai eseguiti sulla tua GPU. Un'app desktop, un'interfaccia a riga di comando (CLI) e un componente aggiuntivo (side-car) per agenti, tutto basato su una singola libreria Rust.

Il side-car consente a un agente di pianificare una sessione a pagamento con un budget definito dall'utente, noleggiare le GPU e assegnare una coda di attività a un processo separato. Il processo mantiene attive tutte le risorse del modello, rivede solo i controlli che hanno fallito e arresta il pod quando la coda è vuota. Un watchdog termina il pod alla scadenza del piano, anche se tutto il resto è terminato.

## Stato

Testato con successo il 2026-10-02 e il 2026-10-03, per un totale di circa 5 dollari:

- **Frontier:** 4 × RTX PRO 6000 (384 GB) che eseguono Qwen3-Coder-480B (AWQ a 4 bit) su SGLang, pronti in 22 minuti, quindi 31 trasferimenti completati in 20 secondi, per 3,59 dollari.
- **Dati Swarm:** un modello caricato serve centinaia di agenti contemporaneamente. Il modello da 480B ha raggiunto 4.059 token/s con 512 agenti; un modello da 30B su una singola scheda ha raggiunto 10.147 token/s con 256 agenti.
- **Runner:** una coda con dipendenze e invii di revisione, ha funzionato senza alcun intervento manuale; il pod si è arrestato al termine dei trasferimenti.
- **Garanzia:** la GPU locale è rimasta inattiva durante tutte le esecuzioni.

In uso quotidiano dal 2026-10-07 da due progetti contemporaneamente, ciascuno nel proprio spazio: esecuzione di training per aspire-si su `job` pod e rendering di tracce audio per ai-jam-sessions su `jam` pod.

Il 2026-10-11, un pod SGLang ha registrato `Application startup complete`, quindi `/health` è rimasto inattivo per 7 minuti. Le GPU hanno operato vicino al 100% a circa 100 W e `/v1/models` ha restituito 200. I pod attivi in quella settimana utilizzavano CUDA 13.0. Quel pod è stato configurato per utilizzare CUDA 13.2. Non è stato dimostrato che 13.2 sia la causa; il flag consente a un profilo di rimanere sulla versione che ha funzionato. Questa versione segnala l'errore e non considera un modello pronto finché un completamento non restituisce una scelta. Un profilo può anche fissare la versione CUDA dell'host in modo specifico.

Non è stato ancora testato su un dispositivo in leasing per questa versione: la funzione di verifica con un singolo token, la...
i report `engine_unresponsive` e `engine_hung` e un comando che invia
`gpu.allowedCudaVersions`. Tali funzionalità sono verificate tramite test che simulano una sessione SSH fittizia.
e un file RunPod fittizio. Nessun pod è stato avviato per ricontrollarli. Il v1 documentato da RunPod.
`POST /pods` enum, consultato l’11 ottobre 2026, continua a fermarsi alla versione 13.0. Le versioni 13.1 e 13.2 si trovano in offrig.
elenco perché quel podcast di scrittori è stato pubblicato il 13 febbraio. Un piano ancora invia solo
`gpu.minCudaVersion`.

Creato e testato, in attesa di una decisione: preparazione dei pesi di Frontier su un volume di rete, circa 21 dollari al mese (vedere [Preparazione](#caricamento-dei-pesi-su-un-volume-di-rete)).

Successivamente: la prima vera coda di attività, pianificata completamente prima del lancio; il codice viene compilato e testato sul pod.

## Cosa fa

Da una singola finestra (o da un singolo comando), offre:

1. mostra il tuo saldo RunPod, i prezzi delle GPU in tempo reale e per quanto tempo il saldo durerà;
2. avvia un pod per un livello, da una singola scheda piccola su Ollama fino a 4 × RTX PRO 6000 su SGLang, e carica i suoi modelli sul pod;
3. apre un tunnel SSH verso il pod;
4. aggiunge i modelli del pod a Zed come provider separato;
5. esegue sette controlli per garantire che i modelli non vengano eseguiti su questa macchina;
6. arresta il pod o lo termina dopo un periodo di tempo con tutte le GPU inattive.

Tramite il componente aggiuntivo, un agente pianifica anche sessioni in base a un budget, mantiene la memoria del progetto tra le operazioni di compattazione e i riavvii ed esegue le code di trasferimento senza supervisione (vedere [Il componente aggiuntivo](#il-side-car-per-gli-agenti)).

## La garanzia e come viene mantenuta

- **Il server del modello è irraggiungibile tranne tramite il tunnel.** Il pod esegue un motore fissato, Ollama (`ollama/ollama:0.35.0`) o SGLang (`lmsysorg/sglang:v0.5.20-cu130`), vincolato al loopback del pod e il pod espone solo `22/tcp`. Non esiste un endpoint HTTP pubblico da trovare o sfruttare. Un modello non può spostare il motore fuori dal loopback.
- **Zed comunica con il tunnel, sulla sua porta.** Il tunnel è in ascolto sulla porta `127.0.0.1:11435`. Il tuo Ollama locale è sulla porta `11434`. offrig si rifiuta di posizionare il tunnel sulla porta `11434`, quindi un tunnel non funzionante non può raggiungere il server locale: la richiesta fallisce.
- **Zed non cambia mai provider.** I modelli del pod sono un provider `offrig` separato in Zed. Se il pod è inattivo, la selezione di uno di essi genera un errore; Zed non prova un altro provider.
- **I pesi non esistono mai localmente.** I modelli vengono caricati sul pod, dal pod (o scaricati da lì da Hugging Face, o letti da un volume di rete preparato).

La guardia verifica questo ogni volta, sulla base dei dati che offrig può osservare:

| Controllo | Fallisce quando |
|---|---|
| Il tunnel evita la porta locale di Ollama | la porta del tunnel è 11434 |
| Zed invia i modelli del pod attraverso il tunnel | L'URL del provider di Zed è qualsiasi cosa, tranne il tunnel |
| Ollama del pod non è esposto a Internet | il pod mappa pubblicamente la porta 11434 |
| Il tunnel termina sul pod | l'elenco dei modelli attraverso il tunnel è diverso dall'elenco letto sul pod tramite SSH |
| I modelli del pod non si trovano su questa macchina | un modello del pod esiste anche in Ollama locale |
| Nessun modello del pod condivide un nome con un modello Zed locale | un nome nel provider offrig è anche nell'elenco di Ollama locale di Zed |
| Ogni modello offerto da Zed è presente sul pod | Zed offre un modello che il pod non ha |

## Installazione

Richiede Windows con OpenSSH (integrato), Zed se si desidera che i modelli siano in un editor e un account RunPod.

1. Scarica `offrig-<version>-windows-x64.zip` da
[Releases](https://github.com/mcp-tool-shop-org/offrig/releases), verifica che corrisponda alla
versione di rilascio `SHA256SUMS` e decomprimilo nella tua `PATH`. Contiene `offrig.exe` (la CLI),
`offrig-app.exe` (l'app) e `offrig-mcp.exe` (il componente aggiuntivo).
Per compilare dal codice sorgente: `cargo build --release`, con Rust 1.98.1 (versione specificata in
`rust-toolchain.toml`).
2. Inserisci la tua chiave API RunPod nella variabile d'ambiente dell'utente `RUNPOD_API_KEY`.
3. Aggiungi la tua chiave pubblica SSH nelle impostazioni dell'account di RunPod. offrig utilizza
`~/.ssh/runpod_rustline` se presente, altrimenti `~/.ssh/id_ed25519`.
4. Per gli agenti, registra il componente aggiuntivo con Claude Code nell'ambito dell'utente:
`claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Apre lo spazio di archiviazione di un progetto solo al primo utilizzo, quindi è innocuo nei progetti che non lo utilizzano mai.

Il [manuale](https://mcp-tool-shop-org.github.io/offrig/handbook/) illustra un
primo pod, il componente aggiuntivo, la configurazione, i canali e i pod di lavoro.

## Utilizzo

**App:** avvia `offrig-app`, seleziona un profilo, premi **Avvia pod**. Quando è pronto, i
modelli compaiono nel pannello degli agenti di Zed come "RunPod · …". Riavvia Zed una volta dopo il primo
avvio in modo che rilevi `OFFRIG_API_KEY`.

**CLI:**

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

`offrig --help` e `offrig <command> --help` elencano tutti i comandi e le opzioni. Il blocco
sopra è l'insieme giornaliero. `offrig init`, `offrig models`, `offrig zed` e
`offrig up --no-zed` sono presenti nel [manuale](https://mcp-tool-shop-org.github.io/offrig/handbook/usage/).

### Output, codici di uscita ed errori

- **Livelli di log:** `-q` stampa solo gli errori e i risultati di un comando; `-v` aggiunge ogni
chiamata RunPod e il relativo tempo di esecuzione; `--debug` aggiunge i corpi delle risposte non riuscite e le catene di errori complete.
La chiave API viene oscurata a tutti i livelli.
- **Codici di uscita:** `0` successo, `1` qualcosa da correggere dalla tua parte (argomenti, configurazione, un
rifiuto di una protezione o di un budget, una chiave mancante), `2` un errore di runtime (RunPod, rete, ssh,
timeout, nessuna capacità).
- **Errori del componente aggiuntivo** sono risultati, mai errori di protocollo: `ok:false` con un `code` stabile,
il testo `error`, un `next_action` e `retryable`. I codici sono elencati nel
[riferimento del manuale](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## Il side-car (per gli agenti)

`offrig-mcp` è un server MCP che un agente come Claude Code utilizza come strumento. Mantiene un database per progetto in `<project>/.offrig/offrig.db` che sopravvive a ogni pod, quindi una sessione sopravvive alla compressione o a un riavvio senza dover spiegare di nuovo tutto.

| Strumento | Cosa fa |
|---|---|
| `offrig_status` | Il progetto, il budget, il saldo RunPod e le risorse disponibili, i pod di offrig, ogni piano aperto con il suo `plan_id`, il nome del canale e del pod, la coda di handover con il lavoro obsoleto contrassegnato. Ogni pod include `gpu_count` e `gpus` (`2x <type>`) dal record RunPod, oltre al `planned_gpu_count` del piano. |
| `offrig_offers` | Offerte di GPU attive per un numero di GPU |
| `offrig_plan` | Calcola il costo di una sessione nel peggiore dei casi (prezzo live x (ore massime + 780 s di tempo di auto-arresto), arrotondato per eccesso ai centesimi); rifiuta se supera il budget disponibile. La risposta indica il `ssh_alias` del canale e il `pod_name` che verrà creato al lancio, e il `container_disk_gb` che verrà richiesto (l'opzione `container_disk_gb` sovrascrive il profilo; vedi "Disco del container"). Le opzioni `max_price_hr` e `no_fallback` limitano le GPU che può affittare (vedi "Fissare l'hardware di un piano"); l'opzione `wait_minutes` imposta per quanto tempo il lancio riprova quando non ci sono risorse disponibili (vedi "In attesa di risorse"). |
| `offrig_memory_search` | Cerca nella memoria del progetto attivo, ogni risultato con sorgente e data |
| `offrig_memory_record` | Aggiunge una breve descrizione, un vincolo, una decisione, un fatto o un punto di controllo; le modifiche sono sovrascritture con una motivazione |
| `offrig_handoffs` | Mette in coda le attività da assegnare (ognuna richiede un controllo di accettazione; controlli deterministici opzionali), le elenca, visualizza in anteprima i blocchi delle attività, mostra l'output migliore di un'attività (anche scritto in `.offrig/out/`), registra i risultati (completato, non valido, violazione, errore, riprova con feedback) |
| `offrig_launch` | **Spende.** Accetta solo un `plan_id`: si impegna per il caso peggiore, attende il noleggio delle GPU senza fare nulla, avvia il pod, apre il tunnel, carica i modelli, avvia il watchdog. È idempotente per piano. Rifiuta se lo spazio ha già un piano o un pod attivo: `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | Progresso del lancio (mentre il pod si avvia, il passaggio deriva dallo stato del pod al momento della chiamata e `progress.pod_status` è lo stato v2 non elaborato; ogni tentativo di capacità viene conteggiato in `progress.capacity_wait`), il tipo di GPU, `gpu_count`, `gpus`, `planned_gpu_count` e, una volta che ssh è attivo, `measured_gpu_count` (una lettura senza schede è `0`). `gpu_mismatch` è vero quando il record o la misurazione differiscono dal piano. La versione CUDA dell'host viene misurata con `nvidia-smi`, con una voce `warnings` quando l'host è più vecchio della soglia CUDA del piano. Per una ricetta, `ready` significa che un completamento ha restituito una scelta. `engine_hung` e `engine_unresponsive` lasciano il pod attivo. Liveness del watchdog, minuti rimanenti, spesa finora |
| `offrig_ask` | Un ciclo di un'attività sul modello del pod, il contesto viene creato dall'archivio del progetto; la risposta viene restituita come output non attendibile |
| `offrig_run` | Avvia un processo separato che mantiene attive tutte le risorse del modello: elabora ogni attività pronta, rivede al massimo due volte in caso di errori, fornisce i risultati alle attività dipendenti, quindi arresta il pod quando la coda è vuota (a meno che non sia `keep_pod`). Il lavoro che il codice non può controllare viene messo in attesa per la revisione |
| `offrig_put` | Copia un file o una directory locale in un pod di lavoro (scp); i percorsi relativi del pod sono sotto `/workspace/job`. Opzionale `plan_id` (vedi sotto) |
| `offrig_exec` | Esegue un comando bash su un pod di lavoro, in modo separato in modo che sopravviva al side-car (`start`), segnala se è in esecuzione o terminato con il suo codice di uscita e l'ultima parte del log (`status`; `save_log` copia anche l'intero log in un file locale), lo termina (`stop`) o esegue un comando breve ora e restituisce il suo stdout, stderr e codice di uscita (`run`, `timeout_secs` predefinito 30, massimo 120). Opzionale `plan_id` (vedi sotto) |
| `offrig_get` | Copia un file o una directory da un pod di lavoro, creando le cartelle principali locali mancanti; farlo prima dell'arresto, che elimina il disco del pod. Opzionale `plan_id` (vedi sotto) |
| `offrig_shutdown` | **Distrugge il pod.** Lo termina e chiude i registri del piano con la spesa misurata; rifiuta se le attività sono in corso, a meno che non venga fornita una motivazione. Rimuove il blocco `~/.ssh/config` dello spazio quando nomina quel pod (`ssh_block_removed`) |
| `offrig_complete` | **Spende.** Un singolo completamento di chat OpenRouter, solo per un progetto e un modello consentiti. Si impegna per il peggiore dei casi rispetto al budget, scrive la risposta in un file di progetto (sostituendo quel percorso se esiste già) e registra la spesa effettiva. |

**A quale piano di lavoro agisce uno strumento di lavoro.** `offrig_put`, `offrig_exec` e `offrig_get` accettano un `plan_id` opzionale. Con esattamente un piano di lavoro aperto e nessun `plan_id`, lo utilizzano, come prima. Con più di un piano di lavoro aperto e nessun `plan_id`, rifiutano e elencano i piani aperti (id, profilo, nome del pod): non indovinano mai. Con un `plan_id`, agiscono solo sul pod di quel piano e solo dopo aver verificato che il nome del pod sia quello di cui quel piano è proprietario (un altro pod dello spazio viene rifiutato, non solo di un altro spazio). Ogni risposta dello strumento di lavoro e `offrig_job` indica il `project` e il `plan_id` su cui ha agito (le risposte dello strumento di lavoro indicano anche il `lane`); `offrig_status` indica il `project` ed elenca ogni piano aperto con il suo `plan_id`, lo spazio e il nome del pod.

**Task e skill.** Un client che supporta MCP Tasks può eseguire `offrig_launch` come task: `tasks/get` mostra lo stesso stato di `offrig_job` e `tasks/cancel` annulla un avvio ancora in corso (tramite `offrig_shutdown`); per fermare una sessione già pronta, chiama `offrig_shutdown`. La scadenza del piano e il watchdog decidono ancora quando un pod si ferma. Il server pubblica anche una skill di procedura, `offrig-session`. I dettagli sono nel [manuale del side-car](https://mcp-tool-shop-org.github.io/offrig/handbook/side-car/#tasks).

I ruoli provengono da Role OS (cartelle e schede di avvio) più quattro ruoli di gioco forniti qui nei formati di Role OS: game-designer, systems-designer, narrative-designer, lore-keeper. Il limite di budget è impostato solo da un essere umano:

```text
offrig budget --provider runpod 30       RunPod's cap (run in the project directory)
offrig budget --provider openrouter 8    OpenRouter's cap
offrig budget --show                     every cap, beside each provider's own balance
```

Ogni avvio di un side-car avvia un **watchdog**: un processo separato che termina il pod entro
la scadenza del piano (tempo previsto + ore massime), anche se l'agente, la sessione o il
side-car non sono più attivi. Non agisce mai su una ricerca fallita, termina esattamente una volta, chiude i
conti e registra i dati in `.offrig/watchdog-<plan>.log`. Se la preparazione di un pod in affitto fallisce
prima che il motore sia attivo, il side-car lo termina invece di lasciarlo in esecuzione con addebito.
`engine_hung` e `engine_unresponsive` sono solo a scopo di report su ogni interfaccia: il side-car,
la CLI e l'app mantengono il pod attivo e non indicano che è pronto. Sulla CLI e sull'app, il
comando si interrompe con un errore. Sul side-car, il risultato dell'avvio rimane positivo, quindi
nessuno elimina il pod, il piano rimane attivo e il lavoro rimane `running`. Un processo del motore che si è già interrotto non è incluso in questo report. Sul side-car, si tratta di un avvio fallito e il compensatore elimina il pod. Un pod avviato con `offrig up` o dall'app
non ha un watchdog. Quando il comando si interrompe con un errore (un blocco, un motore non reattivo,
un motore che si è interrotto o un timeout), il pod continua a essere addebitato fino a quando `offrig down <profile>
--yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED` non viene eseguito: in tal caso, viene terminato su tutte le interfacce.

Il progetto e le relative prove sono disponibili in [docs/sidecar-design.md](docs/sidecar-design.md).

### Indice del progetto e verificatore

```text
offrig index src docs                 chunk this project; only changed files are re-embedded
offrig index --status                 counts, model, dimension; changes nothing
offrig index --rebuild                drop every vector and embed again
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on
offrig verify calibrate --think-policy grounded=off,reasoning=escalate:off>on --model gemma4:31b gold/grounded.jsonl
offrig verify calibrate --report-only .offrig/out/calibrate-<model>-<time>
```

`offrig index` è integrato con **EmbeddingGemma 2** di Google DeepMind (Apache-2.0, non limitato),
eseguito sulla CPU da [`tools/embed-serve`](tools/embed-serve/) (`embed_url`, predefinito
`http://127.0.0.1:11490`), mai sul server condiviso sulla porta 11434 e mai su una GPU. Ha sostituito
nomic-embed-text nella versione 1.2.0 dopo un benchmark preregistrato sulle query interne dello studio
(dense nDCG@10 +0.112); un indice creato con nomic non è accettato fino a `offrig index --rebuild`. Una volta che
il progetto ha un indice, `offrig_memory_search` per impostazione predefinita utilizza un approccio ibrido (parola chiave più significato).
`offrig verify calibrate` valuta un modello di loopback locale come strumento di verifica, con una sola affermazione valida
per chiamata, utilizzando le prove dell'affermazione stessa. Un nome di modello che contiene "cloud" non è accettato.
`--num-ctx` per impostazione predefinita utilizza `auto` e definisce una finestra per l'esecuzione. `--keep-thinking`
scrive il testo di ragionamento in `thinking.jsonl` solo nella directory di esecuzione. `--jobs <N>`
(da 1 a 8, predefinito 1) mantiene fino a N affermazioni in corso contemporaneamente e le conferma comunque in
ordine di file: il server deve consentire N in parallelo (`OLLAMA_NUM_PARALLEL`, che per impostazione predefinita
è 1 in Ollama) o le richieste aggiuntive vengono messe in coda, la memoria KV richiesta aumenta con il numero di slot paralleli e la concorrenza può modificare i risultati fino a quando non vengono misurati. Il
[manuale di riferimento](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/)
elenca i flag.

### Canali: un side-car per progetto, senza conflitti

Due progetti possono eseguire "side-car" contemporaneamente sullo stesso account RunPod. Ogni progetto ottiene la propria
**"corsia"**: un alias SSH, una porta tunnel e un tag del nome del pod che nessun altro progetto condivide.

| | Canale standard (la CLI, l'app, Zed) | Canale di un progetto |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Porta del tunnel | `11435` (runner `11436`) | prima porta libera di `11500`, `11502`, ... (runner: la porta sopra) |
| Porta del side-car (driver della shell) | nessuna | `11700` + lo slot del canale: `11700`, `11701`, ... |
| Nome del pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Blocco SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` deriva dal nome della cartella del progetto (`aspire-si`, `ai-jam-sessions`), con un hash breve aggiunto quando due progetti condividono lo stesso nome di cartella. Il canale di un progetto viene allocato la prima
volta che pianifica una sessione, scritto in `lanes.toml` nella directory di configurazione di offrig e mantenuto:
lo stesso progetto ottiene lo stesso canale dopo ogni riavvio. L'allocazione utilizza un file di blocco e
scrive il registro in modo atomico, in modo che due side-car avviati contemporaneamente non condividano mai un tag, un alias o una porta. Nessun canale può essere `11434` (la porta di Ollama locale): l'intervallo inizia da
`11500` e una configurazione modificata per indicare diversamente viene rifiutata. Un piano registra il suo canale e
il suo avvio, il runner, il watchdog e l'arresto utilizzano tutti quel canale, non la configurazione globale.

Un side-car corrisponde, elenca o arresta solo i pod denominati per il proprio canale. Il canale di un altro
progetto, i pod del canale standard `offrig-<profile>` e qualsiasi altro pod nell'account
rimangono invariati: il controllo di avvio per un pod attivo richiede solo
il proprio canale, l'arresto rifiuta un pod il cui nome non è il canale del piano e il controllo degli orfani del tunnel elimina un `ssh` obsoleto solo quando il suo forward e il suo alias sono del canale stesso.
I piani creati prima dell'esistenza dei canali non hanno un canale registrato e continuano a essere eseguiti nel canale standard, quindi un pod avviato con il vecchio schema viene arrestato dallo stesso piano che lo ha avviato.

**La porta del side-car.** `offrig-mcp` comunica tramite MCP su stdio. Un driver della shell che lo mantiene
aperto per un'intera sessione (quando la connessione MCP della sessione stessa è obsoleta) lo mette dietro
una porta HTTP loopback e quella porta era un numero per l'intera macchina (`11439`):
un secondo driver del progetto o qualsiasi altro programma poteva prenderla e il primo side-car si spegneva senza dire nulla. L'impostazione predefinita è ora per progetto, dal canale del progetto, nello stesso modo
in cui è la porta del tunnel: lo slot del canale `i` (porta del tunnel `11500 + 2i`) ottiene la porta del side-car `11700 + i`.
L'intervallo `11700` a `11763` si trova sopra ogni porta del tunnel e del runner che un canale può avere
(`11500` a `11627`), la `11435` e la `11436` del canale standard e la `11434` di Ollama locale,
in modo che una porta del side-car non possa mai essere una porta del tunnel. Non vengono memorizzate nuove informazioni: `lanes.toml` è
invariato e la porta deriva dal canale. `OFFRIG_SIDECAR_PORT` lo sovrascrive comunque; un
valore che non è una porta, è inferiore a 1024 o è `11434`, `11435`, `11436` o qualsiasi cosa nell'intervallo del tunnel del canale viene rifiutato.

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Con `--check`, una porta già in uso è un errore che indica la porta e, quando un side-car "offrig"
risponde, indica il progetto a cui serve: `la porta del side-car 11700 è già in uso: un side-car offrig
sta già servendo il progetto <path>. Interrompilo prima oppure imposta OFFRIG_SIDECAR_PORT su una
porta libera`. Il controllo verifica il modo in cui il driver risponde già (una richiesta come progetto
che nessuno serve, che il driver rifiuta prima di toccare qualsiasi strumento), quindi non cambia nulla
in un side-car in esecuzione. `offrig_status` segnala la `sidecar_port` della "lane".

**Un canale, un pod attivo.** Un canale ha un alias SSH e una porta del tunnel, quindi gestisce un
pod alla volta: un secondo pod nel canale (`offrig-<tag>-job` accanto a `offrig-<tag>-jam`)
riassegnerebbe l'alias a se stesso e invierebbe la `offrig_put`, la `offrig_exec` e la `offrig_get` del primo piano alla macchina sbagliata. `offrig_launch` quindi rifiuta mentre il canale ha
un piano aperto o qualsiasi pod attivo di sua proprietà, con `lane <tag> has a live pod <name> (plan <id>);
shut it down first`, before anything is committed or rented. The plain lane's `offrig up`
e l'app rifiuta allo stesso modo per un pod di un altro profilo (il pod dello stesso profilo viene
ancora riutilizzato). Arresta il primo piano, quindi avvia il successivo.

## Livelli

I profili si trovano in `%APPDATA%\offrig\config.toml` (scritti alla prima modifica). Impostazioni predefinite:

| Profilo | GPU | Modelli | Costo tipico |
|---|---|---|---|
| piccolo | 1 × RTX 2000 Ada / classe A4000 | `qwen3:4b` | circa $0,25/ora |
| medio | 1 × RTX PRO 6000 (96 GB); A100 o H100 80 GB se non ne è disponibile nessuno | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | 2,09 dollari/ora (A100, in caso di fallback, 1,59 dollari) |
| avanzato | 4 × RTX PRO 6000 (384 GB), **SGLang** | Qwen3-Coder-480B AWQ a 4 bit (252 GB), circa 130 GB disponibili per il contesto | $8,36/ora |
| avanzato-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 GB): il percorso del motore di avanguardia, testato in modo economico | circa $1,7/ora |
| avanzato-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 GB): i kernel a 4 bit MoE dell'avanguardia, testati in modo economico | circa $1,7/ora |
| lavoro | 1 × RTX PRO 6000 (96 GB); A100 o H100 80 GB se non ne è disponibile nessuno | nessuno: un **pod di lavoro** esegue il tuo lavoro, non un server di modelli | 2,09 dollari/ora (A100, in caso di fallback, 1,59 dollari) |
| jam | 1 × A40 (48 GB) prima; A6000, A5000, 3090, L4 o 4090 se non ne è disponibile nessuno | nessuno: un **pod di lavoro** per i rendering di canto di ai-jam-sessions (SoulX-Singer) | 0,49 dollari/ora (A40) |

Un profilo con un `recipe` esegue un motore diverso da Ollama: un'immagine bloccata
(`lmsysorg/sglang:v0.5.20-cu130`), un modello di Hugging Face che scarica all'avvio e
argomenti del server aggiuntivi. offrig imposta il parallelismo dei tensori in base al numero di GPU, la lunghezza del contesto in base al profilo e mantiene il motore sul loopback del pod; una ricetta non può
sovrascrivere questi valori. Per un repository protetto, `hf_token_secret` indica un segreto RunPod, a cui si fa riferimento come
`{{ RUNPOD_SECRET_<name> }}`, in modo che il token non entri mai nelle specifiche del pod. L'avvio segnala
i pesi su disco mentre il motore li scarica e si interrompe immediatamente (con il log del motore) se il motore si chiude. Non indica che il motore è pronto quando `/health` risponde e
`/v1/models` che risponde non è comunque un indicatore di prontezza. Dopo che `/health` restituisce 200, offrig
invia un completamento di chat (`max_tokens` 1, `temperature` 0) e attende fino a 30 secondi.
Il passaggio è `ready` solo quando tale risposta contiene una scelta. Se il completamento fallisce o scade, offrig riprova dopo 15 secondi. Un secondo fallimento è il passaggio
`engine_unresponsive`: il lavoro continua a essere eseguito e l'errore e il log del motore sono disponibili nel lavoro. L'azione successiva è `il motore non risponde ai completamenti: interrompi il pod oppure attendi se è ancora in fase di riscaldamento`. offrig non interrompe il pod per questo, né sul side-car, né sulla CLI, né sull'app. Una prima richiesta lenta (acquisizione del grafico, JIT) non è un motivo
per eliminare il download e il caricamento; la scadenza del piano limita comunque i costi.
L'orologio di attesa inizia da `grep -c` di `Application startup complete` sulla stessa lettura ssh
del log, sull'intero log del motore, non solo sulle ultime 40 righe. Un motore "chiacchierone" può spostare la riga di avvio fuori da tale log. Il conteggio avvia comunque l'orologio.
Se tale conteggio è almeno uno e `/health` rimane a zero per 3 minuti, il passaggio è
`engine_hung`: le ultime 40 righe, i secondi trascorsi da quando il conteggio è stato rilevato per la prima volta e
l'azione successiva `shut the pod down: the engine finished startup but never became healthy`.
offrig segnala tale attesa e lascia il pod attivo. L'interruzione del pod dipende da te,
come un errore di corrispondenza del numero di GPU. Un avviso sul numero di GPU non viene aggiunto all'azione successiva, né a quella non reattiva, quindi la frase rimane esatta. Un processo del motore che si è già chiuso non viene segnalato. Sul side-car, il compensatore elimina
quel pod. Sulla CLI e sull'app, il comando si interrompe con l'errore e non lo elimina.

Ogni profilo elenca i tipi di GPU in ordine di priorità. v2 specifica un tipo e non effettua fallback, quindi offrig prova questa lista, in ordine, e non supera mai il prezzo previsto. Quando nessuno è disponibile, un profilo può attendere (`wait_for_gpu_minutes`; il sistema attende fino a 120 minuti): offrig controlla ogni minuto e crea il pod nel momento in cui le GPU diventano disponibili. Durante l'attesa, non viene effettuato alcun noleggio; Ctrl+C o l'opzione "Annulla" dell'app interrompono il processo e, se il catalogo attivo non è disponibile, il sistema riprova a creare il pod ogni minuto. Le configurazioni di grandi dimensioni con più GPU vengono create e eliminate in pochi minuti. I prezzi sono i prezzi di listino del catalogo v2 (`GET /catalog/gpus`), per GPU, moltiplicati per il numero di GPU. Un elenco con "NESSUNO" non indica disponibilità. La pagina dei prezzi pubblici non corrisponde al prezzo disponibile. `CUDA_VERSIONS` è solo il fallback quando l'elenco `cudaVersions` di una GPU viene omesso.

### In attesa di capacità

Un piano ristretto con `no_fallback` o `max_price_hr` spesso non ha capacità, quindi l'avvio
ritenta silenziosamente invece di fallire, non affittando nulla durante il processo. Per quanto tempo attende, in
ordine: la `wait_minutes` del piano (un argomento `offrig_plan`, memorizzato con il piano; `0` fallisce
immediatamente), altrimenti la `wait_for_gpu_minutes` del profilo. Il profilo `job` ha un valore predefinito di 20 minuti.
L'attesa viene ridotta al tempo rimanente del piano meno una riserva di cinque minuti, in modo che non si esegua
mai oltre la scadenza del piano e, poiché durante l'attesa non viene affittato nulla, non aggiunge nulla al
peggiore dei casi previsto. `offrig_launch` segnala `capacity_wait_minutes`; durante l'attesa,
`offrig_job` mostra `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) e un passaggio
che indica quale sia questo controllo. Quando l'attesa termina, l'avvio fallisce con `no capacity`
e non è stato affittato nulla.

### Definizione dell'hardware di un piano

Un profilo elenca i tipi di GPU in ordine di priorità e RunPod seleziona il primo con capacità disponibile, quindi, senza limiti, un piano potrebbe utilizzare una scheda di fallback con meno memoria, un driver più vecchio e un prezzo diverso. Tre limiti consentono di limitare un piano all'hardware che può effettivamente utilizzare. La pianificazione è ancora gratuita; i limiti limitano solo ciò che il piano può noleggiare.

| Limite | Dove | Effetto |
|---|---|---|
| `min_cuda` | profilo (`config.toml`) | La versione CUDA più vecchia dell'host, dall'elenco RunPod (`13.2`, `13.1`, `13.0`, `12.9`, ... `11.8`). Il processo di creazione del pod invia questo valore come `gpu.minCudaVersion`, quindi un host più recente della versione più recente copiata è comunque valido. Quando il catalogo restituisce `cudaVersions` per una GPU, un piano mantiene questo tipo solo se è disponibile una versione a questo livello o superiore. Quando il catalogo omette l'elenco, l'elenco copiato viene utilizzato come fallback. Per un profilo di lavoro, viene utilizzata la versione più recente tra questa e la versione `[profiles.job] min_cuda` dell'immagine. |
| `min_vram_gb` | profilo | La quantità minima totale di VRAM (tutte le GPU del profilo insieme) che un piano accetta. Le offerte inferiori vengono scartate; anche un tipo RunPod che non elenca alcuna memoria viene scartato. |
| `max_price_hr` | argomento `offrig_plan` | Il costo massimo del pod, in totale, può essere di $/ora per tutte le sue GPU (come mostrato nella figura `offrig_offers`). Le offerte superiori a questo valore vengono scartate, così come i tipi di pod che non hanno un prezzo indicato (non possono essere vincolati a un limite massimo). |
| `no_fallback` | argomento `offrig_plan` | È consentita solo la prima famiglia di GPU del profilo. Le due edizioni RTX PRO 6000 Blackwell (Server e Workstation) appartengono a una famiglia; tutte le altre schede, comprese le A100 SXM e PCIe, appartengono a famiglie diverse. |

I campi del profilo sono facoltativi e, per impostazione predefinita, non sono impostati, quindi un `config.toml` scritto da un'istanza precedente viene caricato senza modifiche. Il profilo `job` imposta `min_cuda = "13.0"`.

`cuda_versions` è un elenco esatto facoltativo, oltre a `min_cuda`, ad esempio `cuda_versions = ["13.0"]`. Quando è impostato, il processo di creazione del pod invia questo elenco come `gpu.allowedCudaVersions`, nell'ordine in cui è stato scritto, e non invia anche un valore minimo. `offrig_plan` segnala l'elenco. L'impostazione insieme a `min_cuda`, incluso un valore minimo dell'immagine del lavoro, non è consentita: i due insieme sono ambigui. Ogni versione deve essere una versione che offrig accetta. Questo elenco, dalla versione più recente alla più vecchia, è 13.2, 13.1, 13.0, quindi 12.9 fino a 11.8. 13.2 e 13.1 sono precedenti all'elenco RunPod documentato v1 (letto il 2026-10-11, che si interrompe a 13.0). Sono elencati perché un pod di scrittura del 2026-10-11 è stato posizionato su un host 13.2. Un valore minimo invia comunque solo `gpu.minCudaVersion`.

`offrig_plan` calcola il peggiore dei casi in base a ciò che rimane: `(max_hours + 780 s) x min(max_price_hr, the dearest listed price among the remaining GPUs)` (arrotondato per eccesso ai centesimi; 780 s sono i 120 s di margine di auto-interruzione del pod più 660 s di intervallo di riprova). Senza `max_price_hr`, questo è il prezzo più alto elencato nel profilo, come prima. Un piano senza più risorse disponibili viene rifiutato con il motivo per cui ogni GPU è stata scartata e non viene scritto nulla.

Il piano memorizza l'elenco delle GPU disponibili e, o il limite minimo di CUDA o l'elenco esatto di CUDA, e `offrig_launch` esegue il provisioning solo da ciò che il piano ha memorizzato, mai da una modifica successiva del profilo. `offrig_job` e il risultato dell'avvio del side-car segnalano il tipo di GPU e la versione CUDA dell'host effettivamente utilizzata, in un oggetto `rented`. L'API del pod non segnala la versione CUDA dell'host, quindi una volta stabilita la connessione SSH, l'avvio viene eseguito `nvidia-smi` una volta attraverso il canale e la legge dall'intestazione (`CUDA Version: 12.8` o `CUDA UMD Version: 13.4` sui driver più recenti); `rented.cuda_source` indica `nvidia-smi` o `pod API`. Se la versione CUDA dell'host è precedente al limite minimo del piano, la GPU non è presente nell'elenco del piano o il prezzo è superiore a quello del piano, `offrig_job` restituisce una voce `warnings` e avvia `next_action` con `WARNING`. Nulla viene terminato automaticamente: l'interruzione del provisioning è a discrezione del chiamante (`offrig_shutdown`). Se né l'API del pod né `nvidia-smi` forniscono una versione CUDA, `rented.notes` lo indica e il limite minimo non viene controllato.

### Disco del contenitore

Un pod ha due dischi: il disco del contenitore, locale all'host, e il volume montato in `/workspace`. Su alcuni host, `/workspace` è un file system di rete lento: in un pod di test è stata misurata una velocità di 32 MB/s rispetto ai 354 MB/s sul suo disco del contenitore e non è stato possibile recuperare circa 130 GB di modelli in tempo, mentre il disco del contenitore era solo di 60 GB. La dimensione del disco del contenitore è `container_disk_gb` del profilo (da 30 a 60 GB nei profili predefiniti; il profilo `job` ha 60 GB) e viene inserita nella creazione del pod come `disk`. `offrig_plan` accetta `container_disk_gb` (da 1 a 2000) per sovrascriverlo per un piano; il piano lo memorizza, l'avvio lo invia e la risposta del piano e `offrig_status` mostrano la dimensione effettiva.

- Il disco del container non ha un prezzo: offrig addebita solo il tempo di utilizzo della GPU, quindi, nel peggiore dei casi, il costo del piano è lo stesso indipendentemente dalle dimensioni. RunPod addebita il costo del disco; non è stato verificato se la tariffa che riporta per il pod (`offrig_job` la mostra) include il disco del container.
- offrig non sposta automaticamente i tuoi download. I comandi del job iniziano con `HF_HOME` sul volume `/workspace` (`/workspace/hf`); per utilizzare il disco del container, imposta il tuo volume (`HF_HOME=/root/hf python ...`) nel comando.
- Il disco del container viene eliminato insieme al pod, come il volume senza un volume di rete: copia i risultati con `offrig_get` prima di `offrig_shutdown`.
- Il limite da 1 a 2000 è un controllo di sanità mentale di offrig per evitare errori di battitura; il limite effettivo di RunPod non viene verificato.

### Pod di lavoro

Un profilo con un `job` noleggia una GPU per un lavoro che viene eseguito su di essa, come un ciclo di addestramento, anziché per servire un modello. Il suo pod esegue un'immagine PyTorch predefinita (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 per Blackwell) con sshd e nient'altro:

- Non serve alcun modello, quindi non c'è alcun tunnel e nulla è collegato a Zed. sshd non consente alcun tipo di inoltro (`AllowTcpForwarding=no`); l'unico modo per accedervi è tramite ssh al pod.
- Un profilo di job non elenca alcun modello e non può avere anche una ricetta; il controllo della configurazione rifiuta entrambi.
- `offrig up` e l'app rifiutano un profilo di job prima di noleggiare qualsiasi cosa. Il processo viene eseguito tramite il side-car: `offrig_plan profile=job`, `offrig_launch`, quindi `offrig_put`, `offrig_exec` e `offrig_get`. Il lancio è pronto quando sshd risponde.
- Un comando viene eseguito in modo indipendente sul pod (`setsid nohup`) in `/workspace/job`, quindi sopravvive al side-car e alla sessione ssh. Viene inviato come base64, quindi nulla al suo interno viene letto dalla shell ssh. Il suo log e lo stato di uscita vengono salvati in `/workspace/offrig/jobs/`. I download di Hugging Face vengono salvati in `/workspace/hf` sul volume del pod.
- `offrig_exec action=run` è per controlli rapidi (`ls`, `nvidia-smi`), non per il lavoro: esegue il comando fino al completamento sotto `timeout` (predefinito 30 s, massimo 120 s) e restituisce `stdout`, `stderr`, `exit_code` e `timed_out`. L'output viene troncato agli ultimi 64 KB di ogni flusso (`truncated`) ed è un output del pod non attendibile. Un comando che richiede più tempo è un `start`.
- La coda del log di un job ha le barre di avanzamento compresse: i ridisegni in stile tqdm uniti da ritorni a capo mostrano solo l'ultimo frame. `offrig_exec action=status save_log=<local path>` copia anche l'intero log del job, così come è stato scritto, in un file locale (vengono create le cartelle principali), in modo che la coda possa rimanere breve.
- L'immagine è una build CUDA 12.8, quindi un profilo di job indica la versione CUDA host più vecchia su cui viene eseguito (`min_cuda = "12.8"`) e il pod viene creato con tale valore minimo come `gpu.minCudaVersion`. In caso contrario, un host con un driver più vecchio avvia il pod e torch non trova alcuna GPU, dopo che il noleggio è iniziato. Il lavoro stesso potrebbe richiedere più risorse rispetto all'immagine: il profilo `job` imposta anche `min_cuda = "13.0"` sul profilo (vedi sopra), perché i job che esegue installano una versione corrente di vLLM, il cui PyTorch è una build CUDA 13.
- Budget, piano, watchdog e arresto funzionano come per qualsiasi altro profilo. Copia i risultati prima di `offrig_shutdown`: il disco del pod viene eliminato insieme ad esso.
- `jam` è il profilo di job che ai-jam-sessions utilizza per rendere le sue esecuzioni vocali: SoulX-Singer richiede molte meno risorse di una scheda di addestramento, quindi noleggia una scheda economica da 24-48 GB. La configurazione e la sessione si trovano in tale repository (`docs/vocal-offrig.md`); offrig non sa nulla delle esecuzioni vocali.

### Caricamento dei pesi su un volume di rete

Un profilo di ricetta scarica le sue ponderazioni a ogni lancio: per il progetto di frontiera, ciò richiedeva circa 20/22 minuti per essere pronto (252 GB, $8,36/ora). Lo staging le posiziona su un volume di rete RunPod una sola volta:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- Il volume addebita un costo mensile indipendentemente dal fatto che un pod sia in esecuzione o meno (300 GB per il progetto di frontiera costano circa $21/mese a $0,07/GB), quindi solo un utente esegue lo staging; nessun strumento automatizzato può farlo.
- Un volume si trova in un unico data center, quindi i pod del profilo vengono quindi avviati solo lì e le offerte e i piani hanno prezzi in base a tale posizione. Scegli uno con archiviazione di rete e le GPU del profilo; la console di `offrig gpus` e RunPod mostra dove si trovano.
- Il download viene eseguito sul pod GPU più economico disponibile in tale data center. Il pod viene terminato in caso di successo, di errore o di timeout.
- Il volume viene registrato nel profilo prima dell'inizio del download, quindi uno staging non riuscito non viene mai dimenticato; riesegui per riprendere o `--remove`.
- Un lancio con staging esegue Hugging Face offline, solo quando lo staging è stato completato (un indicatore sul volume). Un volume parzialmente sottoposto a staging scarica invece il resto invece di fallire.

## Sicurezza finanziaria

- Prima di un lancio, offrig mostra la corrispondenza gratuita più economica e il tuo tempo di esecuzione con il pod in esecuzione. Se il tempo di esecuzione è inferiore a un'ora, rifiuta a meno che tu non lo annulli, perché a zero RunPod interrompe ogni pod sull'account, inclusi quelli che offrig non gestisce.
- L'arresto automatico termina il pod dopo 30 minuti con ogni GPU al di sotto del 5% (configurabile o disattivato).
- La chiusura dell'app con un pod in esecuzione chiede se terminarlo o mantenerlo in esecuzione.
- offrig tocca solo i pod che ha nominato: `offrig-<profile>` per la CLI e l'app, `offrig-<tag>-<profile>` per la corsia side-car di un progetto (vedi Corsie). Un side-car non tocca mai i pod di un'altra corsia, i pod della corsia normale o qualsiasi altro pod; questi vengono elencati, ma non modificati.
- Per le sessioni dell'agente, il limite viene applicato prima di qualsiasi spesa: il peggiore dei casi di un piano (prezzo corrente × (ore massime + 780 s di coda di arresto automatico), arrotondato per eccesso ai centesimi) viene preso in considerazione rispetto al budget impostato dall'utente e rifiutato se supera tale budget, e un lancio richiede solo un ID di piano, quindi un agente non può impostare il proprio prezzo.
- Ogni lancio side-car ha un watchdog che termina il pod alla scadenza del piano e il runner arresta il pod non appena la sua coda è vuota.

## Cosa cambia sulla tua macchina

| Cosa | Dove | Annulla |
|---|---|---|
| Provider Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; la prima versione originale viene conservata come `settings.json.offrig.bak` |
| Modello predefinito di Zed (solo se richiesto) | stesso file | `offrig zed-remove` ripristina il valore predefinito precedente |
| `OFFRIG_API_KEY` (segnaposto; Zed richiede una chiave) | ambiente utente | `setx OFFRIG_API_KEY ""` o rimuovilo nelle proprietà di sistema |
| Alias SSH `offrig` | `~/.ssh/config`, tra i marcatori `# >>> offrig:offrig >>>` | elimina il blocco contrassegnato |
| Alias SSH `offrig-<tag>`, uno per progetto avviato da un side-car | `~/.ssh/config`, tra i marcatori `# >>> offrig:offrig-<tag> >>>` | elimina il blocco contrassegnato |
| Canali di progetto | `%APPDATA%\offrig\lanes.toml` (percorso del progetto, tag, alias, porta del tunnel) | elimina la voce del progetto mentre nessun pod è in esecuzione nel suo canale, o l'intero file |
| Chiavi host del pod | `~/.ssh/known_hosts_offrig` | elimina il file |
| Impostazioni | `%APPDATA%\offrig\config.toml` | elimina il file |
| Pesi caricati (solo con `offrig stage --yes`) | un volume di rete RunPod `offrig-<profile>`; fatturazione mensile | `offrig stage <profile> --remove --yes` |

Commenti e layout nelle impostazioni di Zed vengono conservati: le modifiche passano attraverso un albero di sintassi JSONC.

## Modello di minaccia

- **Chiave API RunPod.** Viene letta da `RUNPOD_API_KEY`; non viene mai scritta su disco o nei log. Il provider Zed non è intenzionalmente denominato `runpod`, perché in tal caso Zed leggerebbe `RUNPOD_API_KEY` e lo invierebbe al server del modello.
- **Server del modello.** Accessibile solo tramite SSH con la tua chiave. L'accesso tramite password è disabilitato nel pod e sshd consente solo il port forwarding locale.
- **Chiavi host.** Associate a ciascun endpoint in un file separato "known-hosts". offrig dimentica una chiave solo quando l'endpoint del pod cambia, perché RunPod riutilizza le coppie ip:porta tra i pod.
- **Iniezione di comandi shell.** I nomi dei modelli vengono convalidati rispetto alla sintassi dei nomi di Ollama prima di raggiungere una shell remota.
- **Tunnel orfani.** Se offrig si interrompe, il suo `ssh` potrebbe continuare a mantenere aperta la porta. Al successivo avvio, offrig lo chiude, ma solo se l'ascoltatore sta effettivamente utilizzando le specifiche di port forwarding esatte di offrig `ssh.exe`. Qualsiasi altra cosa sulla porta viene rifiutata e non chiusa.
- **Nessuna telemetria.** offrig comunica solo con l'API di RunPod, il tuo pod e la tua istanza locale di Ollama (per confrontare le liste dei modelli).

## Test

`cargo test --workspace` esegue più di 250 test, coprendo almeno il 90% delle righe (il CI fallisce al di sotto di tale soglia):

- **La libreria principale:** analisi RunPod, specifiche del pod per entrambi i motori, configurazione SSH, modifiche JSONC di Zed, regole di guardia, logica di costo e inattività, lo store e le sue migrazioni, ruoli, assemblaggio del contesto, controlli deterministici, le decisioni del runner, il watchdog e la fase di test, inclusa una simulazione RunPod che dimostra che una fase fallita termina il suo pod.
- **L'app:** gestione dello stato più test dell'interfaccia utente tramite clic nell'ambiente di test di egui.
- **La CLI:** codici di uscita, livelli di log e il fatto che la chiave API non compaia mai nell'output.
- **Il side-car:** test end-to-end tramite stdio rispetto a una simulazione RunPod, il processo reale del watchdog e il processo reale del runner rispetto a un modello di pod simulato; ogni errore dello strumento restituisce un codice.

`scripts/verify.sh` (o `scripts/verify.ps1`) esegue il controllo del formato, clippy, i test e un test di base di ogni binario in un unico comando. Il CI esegue anche `cargo deny`, una scansione OSV di `Cargo.lock`, la copertura su Codecov e `atlas check`.

### Registrazione del test in tempo reale (02-10-2026, livello medio, A100 80 GB, circa 0,45 $)

- Pod attivo in circa 80 secondi; sshd, tunnel e l'Ollama 0.35.0 del pod rispondono.
- 97 GB di modelli caricati sul pod a circa 150–250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` e `gpt-oss:120b` hanno entrambi risposto a una chat in streaming con una chiamata di strumento corretta tramite il tunnel. Hanno utilizzato 36 GB e 64 GB della VRAM del pod; l'Ollama locale non ha caricato nulla e non era affatto sulla GPU locale.
- Tutti e sette i controlli di sicurezza sono stati superati, sia dalla CLI che dall'app.
- Un CLI interrotto bruscamente ha lasciato il suo `ssh` in attesa sulla porta; l'esecuzione successiva l'ha recuperata.
- Il tunnel, i controlli, il test del modello e lo spegnimento dell'app sono stati eseguiti tramite i suoi pulsanti.

Bug trovati durante l'esecuzione live, ora corretti e coperti: un elenco `&&` in background ha mantenuto aperto l'stdout di ssh e ha bloccato l'avvio del caricamento; l'elenco dei pod mancava dei tipi di GPU senza `includeMachine=true`; il controllo di avvio ha contato due volte il prezzo di un pod in esecuzione.

### Prova del side-car (2026-10-03, livello piccolo, RTX 2000 Ada, $0,08 prenotato)

L'installato `offrig-mcp` viene eseguito tramite stdio, nel modo in cui un agente lo chiama:

- `offrig_plan` ha stimato 0,5 ore a 0,15 $ nel caso peggiore; `offrig_launch` ha confermato, ha avviato il watchdog e una seconda chiamata ha restituito lo stesso lavoro. È stato affittato un pod, a 0,24 $/ora.
- SSH attivo 100 secondi dopo l'affitto, `qwen3:4b` scaricato, pronto a 150 secondi.
- `offrig_ask` ha eseguito un passaggio di consegne di un game designer in 46 secondi; la risposta ha soddisfatto il controllo di accettazione e ha mantenuto la restrizione di cinque elementi dalla memoria.
- Il pod ha raggiunto Internet (Wikipedia, API GitHub). In base ai dati recuperati dal pod, il modello ha risposto correttamente alle domande attuali; se interrogato a freddo, ha detto di non avere accesso in tempo reale.
- `offrig_shutdown` da un processo side-car appena avviato ha terminato il pod e ha chiuso i conti; il watchdog ha visto la chiusura del piano ed è uscito. La GPU locale è rimasta inattiva durante tutto il processo.

Trovato e corretto: il pod gestiva una richiesta alla volta (`OLLAMA_NUM_PARALLEL=1`); quattro slot hanno gestito 8 richieste parallele da 40 a 102 token/s sulla stessa GPU, quindi ora ogni profilo ha `parallel = 4`. `complete` è stato rifiutato senza motivo (ora per impostazione predefinita indica "controllo di accettazione superato"; i fallimenti devono comunque indicarne uno). Lo stato suggeriva di registrare un breve periodo di tempo durante il quale una sessione era attiva. Il testo che trapela in una risposta viene eliminato e una risposta svuotata indicando di aumentare `max_tokens`.

### Prova del runner (2026-10-03, livello piccolo, RTX 2000 Ada, $0,04 prenotato)

Cinque passaggi, uno dipendente dall'altro, hanno funzionato tramite `offrig_run` senza che nessuno li gestisse:

- Quattro passaggi in esecuzione contemporaneamente su quattro slot (12,9 GB di 16 GB di VRAM); il passaggio dipendente è iniziato nel momento in cui è terminato il passaggio da cui dipendeva e ha utilizzato il suo risultato.
- I tre passaggi i cui controlli coprivano l'accettazione sono stati completati autonomamente; le storie di sfondo rivali (controlli parziali) e la storia (nessun controllo) sono state inviate per la revisione.
- La revisione ha restituito la storia ("il fiume prende il nome dal progetto"); il runner attivo l'ha adottata e l'ha rivista in base al feedback ("fiume Veyl").
- La coda è stata svuotata in 6,5 minuti (6 passaggi, 20.861 token); il runner ha spento il pod da solo.

Appreso: i controlli deterministici verificano la struttura, non la qualità del design. Il modello da 4B ha superato "tre verbi" con verbi deboli, quindi `accept_on_checks` è per il lavoro strutturale e il lavoro di design viene inviato alla revisione. qwen3:4b ha impiegato circa 4.000 token per pensare per ogni turno, anche su tre righe di storia. Una coda mantiene ogni slot occupato solo quando contiene un numero sufficiente di passaggi di consegne indipendenti; una catena di dipendenze viene eseguita uno alla volta.

### Esecuzioni di Frontier e SGLang (03-10-2026, 4,27 $ prenotati)

| Esegui | Pod | Pronto dopo | Coda | Prenotato |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 minuti | 4 passaggi in 15 secondi | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 minuti | 4 passaggi | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 minuti** (252 GB a 278 MB/s, quindi caricamento) | **31 passaggi in 20 secondi** | **$3.59** |
| 30B, scansione a più livelli | 1 × RTX PRO 6000 | 10 minuti (posizionamento lento dei pod) | solo scansione | $0.43 |

- SGLang v0.5.20 (cu130) viene eseguito su Blackwell: attenzione, `awq_marlin` per
Pesi a 4 bit MoE, parallelismo tensoriale tramite PCIe su quattro schede; `/dev/shm` era di 352 GB.
- Il lavoro svolto sulla versione più avanzata è chiaramente superiore a quello delle versioni più semplici: presenta dialoghi più realistici e una
Modulo Rust che è stato compilato e ha superato i suoi tre test (verificati localmente). Il modello da 30 miliardi di parametri FP8
La versione del codice fornita da «run» non è stata compilata correttamente.
- Un singolo modello caricato è sufficiente per gestire l’intero gruppo; non è necessario creare copie. Esecuzione simultanea con
384 token di risposta, numero totale di token al secondo:

| Agenti | 480B su 4 GPU | 30B AWQ su 1 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

La velocità per singolo agente diminuisce all’aumentare del numero di agenti (480B: 88 → 24 token/s a 64), ma la produttività totale continua ad aumentare; i miglioramenti del 480B si stabilizzano dopo 256. La cache KV del modello più avanzato contiene 398.526 token, quindi, con contesti di trasferimento reali di 2-8k token, il modello più avanzato ora elabora 64 richieste contemporaneamente, mentre la versione con una singola scheda SGLang ne elabora 32.

Durante il processo, sono stati individuati e corretti i seguenti problemi: le revisioni hanno aggiunto un riempimento all'output per superare un controllo dell'intestazione (ora un controllo `no_repeats` integrato e le revisioni sono state ristrutturate in loco); l'output richiesto di un ruolo è stato inavvertitamente incluso nei risultati (ora il passaggio definisce il formato); il feedback sull'intestazione indica il formato Markdown; una revisione non modificata si interrompe invece di ripetersi.

Non ancora verificato in ambiente attivo: una chat
inviata direttamente dal pannello dell’agente di Zed (il formato della richiesta utilizzato da Zed viene testato direttamente) e
il controllo di preparazione della versione 1.1.0, il rapporto sugli eventuali problemi e l’indicazione precisa della versione CUDA su un pod in affitto (vedere
[Stato](#stato)).

## Conformità agli standard

Valutazione rispetto agli standard di flusso di lavoro dello studio (0 mancanze, 1 parziale, 2 presenti, 3 esemplari).

- **PIN_PER_STEP: 2.** Le immagini dei pod sono associate a tag di versione (`ollama/ollama:0.35.0`, `lmsysorg/sglang:v0.5.20-cu130`; una configurazione rifiuta `latest`), il compilatore è impostato sulla versione 1.98.1, le dipendenze sono gestite tramite `Cargo.lock` e il motore Atlas è aggiornato alla versione 1.24.0 della flotta. Ogni passaggio di trasferimento registra il modello, l'hash del ruolo e l'hash del prompt. I modelli sono associati tramite tag o ID del repository, e non tramite hash.
- **ANDON_AUTHORITY: 3.** Ogni passaggio interrompe l'esecuzione in caso di errore: un piano i cui pesi superano la capacità del disco viene rifiutato prima di qualsiasi operazione; un download non riuscito interrompe l'avvio; una modifica Zed che non viene riletta non viene scritta; un file di impostazioni danneggiato viene segnalato, ma non riscritto; il sistema CI blocca l'esecuzione in caso di problemi con il formato, clippy, i test, le licenze e gli avvisi.
- **NAMED_COMPENSATORS: 2.** Ogni azione irreversibile ha una corrispondente operazione di annullamento, elencata di seguito.
- **DECOMPOSE_BY_SECRETS: 2.** Un modulo per ogni elemento che cambia per motivi propri: l'API di RunPod (`runpod`), il contenuto del pod (`spec`), il sistema di trasporto (`tunnel`, `remote`), ogni file locale modificato (`sshconfig`, `zed`) e le regole (`guard`, `cost`). Le interfacce utente non contengono alcuna logica oltre alla presentazione.
- **UNCERTAINTY_GATED_HUMANS: 2.** offrig richiede un intervento solo quando l'esito è costoso o comporta una perdita: avvio con un tempo di esecuzione inferiore a un'ora, terminazione di un pod (con indicazione di cosa viene perso) e uscita con un pod ancora attivo e soggetto a fatturazione. Due decisioni spettano esclusivamente a un operatore umano e nessun strumento automatizzato può prenderle: il limite di budget e la preparazione di un volume, che vengono fatturati mensilmente. L'output del trasferimento che non può essere verificato dal codice viene inviato in fase di revisione anziché essere completato.
- **EXTERNAL_VERIFIER: n/a.** Nessuna richiesta specifica.

**Compensatori**

| Azione | Annulla | Stato dopo l'annullamento | Proprietario |
|---|---|---|---|
| Crea un pod (avvia la fatturazione) | `offrig down <profile> --yes`, l'arresto dell'app o l'arresto automatico | pod terminato, fatturazione interrotta | l'operatore che esegue offrig |
| Termina un pod | nessuno per il suo disco; riavvia il profilo e i modelli vengono riscaricati (un volume di rete li conserva) | nuovo pod, stesso profilo | l'operatore |
| Scrivi il provider Zed o il modello predefinito | `offrig zed-remove`, o ripristina `settings.json.offrig.bak` | Zed come prima, offrig | l'operatore |
| Imposta `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` o eliminalo nelle proprietà di sistema | variabile eliminata | l'operatore |
| Scrivi l'alias SSH | elimina il blocco contrassegnato in `~/.ssh/config` | configurazione come prima | l'operatore |
| Alloca una corsia di progetto (la prima `offrig_plan` del progetto) | elimina la voce del progetto da `lanes.toml` quando nessun pod è in esecuzione nella sua corsia; un nuovo piano alloca di nuovo | corsia libera per il riutilizzo; il blocco alias è separato (riga sopra) | l'operatore |
| Scrivi un blocco alias SSH per una corsia (avvio side-car) | `offrig_shutdown` lo rimuove quando nomina il pod del piano (anche dopo un avvio non riuscito); altrimenti, elimina il blocco `# >>> offrig:offrig-<tag> >>>` in `~/.ssh/config` | configurazione come prima; i blocchi delle altre corsie rimangono invariati | l'operatore |
| Scarica un modello sul pod | `ollama rm <model>` sul pod, o termina il pod | modello eliminato | l'operatore |
| Termina un tunnel offrig orfano | nessuno necessario; solo un `ssh` con l'alias e l'inoltro esatti di questa corsia viene terminato, mai un'altra corsia | porta libera | offrig |
| Avvio side-car (`offrig_launch`) | `offrig_shutdown`; automatico se la configurazione fallisce; il watchdog alla scadenza; l'esecutore quando la sua coda è vuota | pod terminato, piano chiuso con spesa misurata | l'agente chiamante, con il watchdog come backup |
| Prepara un volume (`offrig stage --yes`, fattura mensile) | `offrig stage <profile> --remove --yes` | volume eliminato, profilo torna al download | la persona che lo ha preparato |
| Avvia un lavoro su un pod di lavoro (`offrig_exec action=start`) | `offrig_exec action=stop`, o `offrig_shutdown` | lavoro terminato con tutto ciò che ha avviato; il suo log rimane fino alla scomparsa del pod | l'agente chiamante |
| Esegui un comando breve su un pod di lavoro (`offrig_exec action=run`) | solo ciò che fa il comando stesso; terminato da `timeout` al massimo dopo 120 secondi, o da `offrig_shutdown` | il pod come lo ha lasciato il comando | l'agente chiamante |
| Copia file da o verso un pod di lavoro (`offrig_put`, `offrig_get`) | elimina la copia (sul pod, `offrig_exec`; qui, il file) | come prima della copia | l'agente chiamante |

## Layout

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

## Licenza

MIT. Consulta [LICENSE](LICENSE).

---

Realizzato da <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>
