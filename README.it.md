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

[![Punteggio M8ven](https://m8ven.ai/badge/mcp/mcp-tool-shop-org-offrig-790exm?v=3bd60e98b65d7124b74c4126c5ce5ce7)](https://m8ven.ai/mcp/mcp-tool-shop-org-offrig-790exm?s=readme)

Esegui modelli di grandi dimensioni su GPU RunPod noleggiate, con una garanzia: non verranno mai eseguiti sulla tua GPU. Un'applicazione desktop, un'interfaccia a riga di comando (CLI) e un componente aggiuntivo MCP per gli agenti, basati su una singola libreria Rust.

Il componente aggiuntivo consente a un agente di pianificare una sessione a pagamento entro un budget definito dall'utente, noleggiare le GPU e assegnare una coda di attività a un processo separato. Il processo mantiene ogni slot del modello attivo, rivede solo i controlli che hanno fallito e spegne il pod quando la coda è vuota. Un watchdog termina il pod alla scadenza del piano, anche se tutto il resto è terminato.

## Stato

Testato con successo il 2026-10-02 e il 2026-10-03, per un costo totale di circa 5 dollari:

- **Frontier:** 4 × RTX PRO 6000 (384 GB) che eseguono Qwen3-Coder-480B (4-bit AWQ) su SGLang, pronti in 22 minuti, quindi 31 passaggi completati in 20 secondi, per 3,59 dollari.
- **Dati Swarm:** un modello caricato serve centinaia di agenti contemporaneamente. Il modello 480B ha raggiunto 4.059 token/s con 512 agenti; un modello 30B su una singola scheda ha raggiunto 10.147 token/s con 256 agenti.
- **Runner:** una coda con dipendenze e invii di revisione, ha funzionato senza alcun intervento manuale; il pod si è spento al termine.
- **Garanzia:** la GPU locale è rimasta inattiva durante tutte le esecuzioni.

In uso quotidiano dal 2026-10-07 da due progetti contemporaneamente, ciascuno nel proprio ambiente: esecuzioni di addestramento per aspire-si su `job` pod e rendering di musica per ai-jam-sessions su `jam` pod.

Il 2026-10-11, un pod SGLang ha registrato `Application startup complete`, quindi `/health` è rimasto inattivo per 7 minuti. Le GPU hanno operato vicino al 100% a circa 100 W e `/v1/models` ha restituito 200. I pod attivi in quella settimana utilizzavano CUDA 13.0. Quel pod è stato configurato per utilizzare CUDA 13.2. Non è stato dimostrato che 13.2 sia la causa; il flag consente a un profilo di rimanere sulla versione che ha funzionato. Questa versione segnala l'errore e non considera un modello pronto finché un completamento non restituisce una scelta. Un profilo può anche impostare la versione CUDA host in modo specifico.

Not proven on a rented pod for this release: the one-token ready check, the
`engine_unresponsive` and `engine_hung` reports, and a create that sends
`gpu.allowedCudaVersions`. Those paths are covered by tests against a fake ssh session
and a fake RunPod. No pod was started to re-check them. RunPod's documented v1
`POST /pods` enum, read 2026-10-11, still stops at 13.0. 13.1 and 13.2 are in offrig's
list because that writer pod landed on 13.2. A floor still sends only
`gpu.minCudaVersion`.

Creato e testato, in attesa di una decisione: preparazione dei pesi di frontier su un volume di rete, circa 21 dollari al mese (vedere [Preparazione](#staging-weights-on-a-network-volume)).

Successivo: la prima coda di frontier reale, pianificata completamente prima del lancio; passaggi di codice compilati e testati sul pod.

## Cosa fa

Da una singola finestra (o da un singolo comando), offrig:

1. mostra il tuo saldo RunPod, i prezzi attuali delle GPU e per quanto tempo il saldo durerà;
2. avvia un pod per un livello, da una singola scheda piccola su Ollama fino a 4 × RTX PRO 6000 su SGLang, e carica i suoi modelli sul pod;
3. apre un tunnel SSH verso il pod;
4. aggiunge i modelli del pod a Zed come provider separato;
5. esegue sette controlli per garantire che i modelli non vengano eseguiti su questa macchina;
6. spegne il pod o lo termina dopo un periodo in cui tutte le GPU sono inattive.

Tramite il componente aggiuntivo, un agente pianifica anche sessioni in base a un budget, mantiene la memoria del progetto tra le operazioni di compattazione e i riavvii ed esegue le code di passaggi senza supervisione (vedere [Il componente aggiuntivo](#the-side-car-for-agents)).

## La garanzia e come viene mantenuta

- **Il server del modello è irraggiungibile tranne tramite il tunnel.** Il pod esegue un motore con versione fissa, Ollama (`ollama/ollama:0.35.0`) o SGLang (`lmsysorg/sglang:v0.5.20-cu130`), vincolato al loopback del pod, e il pod espone solo `22/tcp`. Non esiste un endpoint HTTP pubblico da trovare o sfruttare. Un modello non può spostare il motore fuori dal loopback.
- **Zed comunica con il tunnel, sulla sua porta.** Il tunnel è in ascolto su `127.0.0.1:11435`. La tua istanza locale di Ollama è su `11434`. offrig rifiuta di posizionare il tunnel su `11434`, quindi un tunnel non funzionante non può raggiungere il server locale: la richiesta fallisce.
- **Zed non cambia mai provider.** I modelli del pod sono un provider `offrig` separato in Zed. Se il pod è inattivo, la selezione di uno di essi genera un errore; Zed non prova un altro provider.
- **I pesi non esistono mai localmente.** I modelli vengono caricati sul pod, dal pod (o scaricati da lì da Hugging Face, o letti da un volume di rete preparato).

I controlli di guardia verificano questo ogni volta, in base ai fatti che offrig può osservare:

| Controllo | Fallisce quando |
|---|---|
| Il tunnel evita la porta Ollama locale | la porta del tunnel è 11434 |
| Zed invia i modelli del pod tramite il tunnel | l'URL del provider di Zed è diverso dal tunnel |
| Ollama del pod non è esposto a Internet | il pod mappa la porta 11434 pubblicamente |
| Il tunnel termina sul pod | l'elenco dei modelli tramite il tunnel è diverso dall'elenco letto sul pod tramite SSH |
| I modelli del pod non sono su questa macchina | un modello del pod esiste anche in Ollama locale |
| Nessun modello del pod condivide un nome con un modello Zed locale | un nome nel provider offrig è anche nell'elenco Ollama locale di Zed |
| Ogni modello offerto da Zed è sul pod | Zed offre un modello che il pod non ha |

## Installazione

Richiede Windows con OpenSSH (integrato), Zed se desideri i modelli in un editor e un account RunPod.

1. Scarica `offrig-<version>-windows-x64.zip` da
[Releases](https://github.com/mcp-tool-shop-org/offrig/releases), verifica che corrisponda al
file `SHA256SUMS` della versione e decomprimilo nella tua `PATH`. Contiene `offrig.exe` (la CLI),
`offrig-app.exe` (l'app) e `offrig-mcp.exe` (il componente aggiuntivo).
Per compilare dal codice sorgente: `cargo build --release`, con Rust 1.98.1 (specificato in
`rust-toolchain.toml`).
2. Inserisci la tua chiave API di RunPod nella variabile d'ambiente dell'utente `RUNPOD_API_KEY`.
3. Aggiungi la tua chiave pubblica SSH nelle impostazioni dell'account di RunPod. offrig utilizza
`~/.ssh/runpod_rustline` se presente, altrimenti `~/.ssh/id_ed25519`.
4. Per gli agenti, registra il componente aggiuntivo con Claude Code nell'ambito dell'utente:
`claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Apre lo spazio di archiviazione di un progetto solo al primo utilizzo, quindi è sicuro nei progetti che non lo utilizzano mai.

Il [manuale](https://mcp-tool-shop-org.github.io/offrig/handbook/) illustra il primo pod, il componente aggiuntivo, la configurazione, i canali e i pod di lavoro.

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
sopra è l'insieme di comandi giornaliero. `offrig init`, `offrig models`, `offrig zed` e
`offrig up --no-zed` sono presenti nel [manuale](https://mcp-tool-shop-org.github.io/offrig/handbook/usage/).

### Output, codici di uscita ed errori

- **Livelli di log:** `-q` stampa solo gli errori e i risultati di un comando; `-v` aggiunge ogni
chiamata a RunPod e il relativo tempo di esecuzione; `--debug` aggiunge i corpi delle risposte non riuscite e le catene di errori complete.
La chiave API viene oscurata a tutti i livelli.
- **Codici di uscita:** `0` successo, `1` qualcosa da correggere dalla tua parte (argomenti, configurazione, un
guard o rifiuto del budget, una chiave mancante), `2` un errore di runtime (RunPod, rete, ssh,
timeout, nessuna capacità).
- **Errori del componente aggiuntivo** sono risultati, mai errori di protocollo: `ok:false` con un `code` stabile,
il testo `error`, un `next_action` e `retryable`. I codici sono elencati nel
[riferimento del manuale](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## Il componente aggiuntivo (per gli agenti)

`offrig-mcp` è un server MCP che un agente come Claude Code chiama come strumento. Mantiene un database per progetto in `<project>/.offrig/offrig.db` che sopravvive a ogni pod,
quindi una sessione sopravvive alla compressione o a un riavvio senza dover spiegare di nuovo tutto.

| Strumento | Cosa fa |
|---|---|
| `offrig_status` | Il progetto, il budget, il saldo e il tempo di esecuzione di RunPod, i pod di offrig, ogni piano aperto con il suo `plan_id`, il canale e il nome del pod, la coda di trasferimento con il lavoro obsoleto contrassegnato. Ogni pod include `gpu_count` e `gpus` (`2x <type>`) dal record di RunPod, oltre al `planned_gpu_count` del piano. |
| `offrig_offers` | Offerte GPU live per un numero di GPU |
| `offrig_plan` | Calcola il costo di una sessione nel suo caso peggiore (prezzo live x ore massime); rifiutato se supera il budget rimanente. La risposta indica il `ssh_alias` del canale e il `pod_name` che verrà creato al lancio, e il `container_disk_gb` che verrà richiesto (l'opzione `container_disk_gb` sovrascrive il profilo; vedi "Disco del container"). Le opzioni `max_price_hr` e `no_fallback` limitano le GPU che può affittare (vedi "Fissare l'hardware di un piano"); l'opzione `wait_minutes` imposta per quanto tempo il lancio riprova quando non c'è capacità (vedi "Attendere la capacità"). |
| `offrig_memory_search` | Cerca la memoria del progetto attivo, ogni risultato con la fonte e la data |
| `offrig_memory_record` | Aggiunge una breve descrizione, un vincolo, una decisione, un fatto o un punto di controllo; le modifiche sono sovrascritture con una motivazione |
| `offrig_handoffs` | Mette in coda i trasferimenti guidati dal ruolo (ognuno richiede un controllo di accettazione; controlli deterministici opzionali), li elenca, visualizza in anteprima i blocchi di ruolo, mostra l'output migliore di un trasferimento (anche scritto in `.offrig/out/`), registra i risultati (completato, non valido, violazione, errore, riprova con feedback) |
| `offrig_launch` | **Spende.** Prende solo un `plan_id`: si impegna per il caso peggiore, attende le GPU affittando nulla, avvia il pod, apre il tunnel, carica i modelli, avvia il watchdog. Idempotente per piano. Rifiutato se il canale ha già un piano o un pod attivo: `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | Progresso del lancio (mentre il pod si avvia, il passaggio deriva dallo stato del pod al momento della chiamata e `progress.pod_status` è lo stato v2 non elaborato; ogni tentativo di capacità viene conteggiato in `progress.capacity_wait`), il tipo di GPU, `gpu_count`, `gpus`, `planned_gpu_count` e, una volta che ssh è attivo, `measured_gpu_count` (una lettura senza schede è `0`). `gpu_mismatch` è vero quando il record o la misurazione differiscono dal piano. La versione CUDA dell'host viene misurata con `nvidia-smi`, con una voce `warnings` quando l'host è più vecchio della soglia CUDA del piano. Per una ricetta, `ready` significa che un completamento ha restituito una scelta. `engine_hung` e `engine_unresponsive` lasciano il pod attivo. Durata del watchdog, minuti rimanenti, spesa finora |
| `offrig_ask` | Un turno di un trasferimento sul modello del pod, il contesto costruito dallo spazio di archiviazione del progetto; la risposta viene restituita come output non attendibile |
| `offrig_run` | Avvia un runner separato che mantiene ogni slot del modello occupato: elabora ogni trasferimento pronto, rivede al massimo due volte in caso di controlli non riusciti, fornisce i risultati ai trasferimenti dipendenti, quindi spegne il pod quando la coda è vuota (a meno che non sia `keep_pod`). Il lavoro che il codice non può controllare attende in revisione |
| `offrig_put` | Copia un file o una directory locale in un pod di lavoro (scp); i percorsi relativi del pod sono sotto `/workspace/job`. Opzione `plan_id` (vedi di seguito) |
| `offrig_exec` | Esegue un comando bash in un pod di lavoro, in modo separato in modo che sopravviva al componente aggiuntivo (`start`), segnala se è in esecuzione o terminato con il suo codice di uscita e la coda del log (`status`; `save_log` copia anche l'intero log in un file locale), lo termina (`stop`) o esegue un comando breve ora e restituisce il suo stdout, stderr e il codice di uscita (`run`, `timeout_secs` predefinito 30, massimo 120). Opzione `plan_id` (vedi di seguito) |
| `offrig_get` | Copia un file o una directory da un pod di lavoro, creando le cartelle principali locali mancanti; fallo prima dello spegnimento, che elimina il disco del pod. Opzione `plan_id` (vedi di seguito) |
| `offrig_shutdown` | **Distrugge il pod.** Lo termina e chiude i libri del piano con la spesa misurata; rifiutato mentre i trasferimenti sono in corso, a meno che non venga fornita una motivazione. Rimuove il blocco `~/.ssh/config` del canale quando nomina quel pod (`ssh_block_removed`) |
| `offrig_complete` | **Spende.** Un solo completamento di chat OpenRouter, solo per un progetto e un modello consentiti. Si impegna per il caso peggiore rispetto al budget, scrive la risposta in un file di progetto (sostituendo quel percorso se è già presente) e registra la spesa effettiva. |

**A quale piano di lavoro si applica uno strumento di gestione dei lavori.** `offrig_put`, `offrig_exec` e `offrig_get` accettano un `plan_id` opzionale. Con esattamente un piano di lavoro attivo e nessun `plan_id`, lo utilizzano come prima. Con più di un piano di lavoro attivo e nessun `plan_id`, lo rifiutano e elencano i piani attivi (ID, profilo, nome del pod): non fanno mai supposizioni. Con un `plan_id`, agiscono solo sul pod di quel piano e solo dopo aver verificato che il nome del pod corrisponda a quello del piano (il pod di un altro "lane" viene rifiutato, non solo di un altro piano). Ogni risposta dello strumento di gestione dei lavori e `offrig_job` indica il `project` e il `plan_id` su cui ha agito (lo strumento di gestione dei lavori indica anche il `lane`); `offrig_status` indica il `project` ed elenca ogni piano attivo con il suo `plan_id`, "lane" e nome del pod.

I ruoli derivano da Role OS (cartelle e schede di avvio) più quattro ruoli di gioco forniti qui nei formati di Role OS: game designer, system designer, narrative designer, lore keeper. Il limite di budget è stabilito solo da un essere umano:

```text
offrig budget --provider runpod 30       RunPod's cap (run in the project directory)
offrig budget --provider openrouter 8    OpenRouter's cap
offrig budget --show                     every cap, beside each provider's own balance
```

Ogni avvio di un processo "side-car" avvia un **watchdog**: un processo separato che termina il pod alla scadenza del piano (tempo previsto + ore massime), anche se l'agente, la sessione o il "side-car" non sono più attivi. Non agisce mai su una ricerca fallita, termina esattamente una volta, chiude i conti e registra i dati in `.offrig/watchdog-<plan>.log`. Se la preparazione di un pod in affitto fallisce prima che il motore sia attivo, il "side-car" lo termina invece di lasciarlo in esecuzione con addebito. `engine_hung` e `engine_unresponsive` sono solo a scopo di segnalazione su ogni interfaccia: il "side-car", la CLI e l'app mantengono il pod attivo e non indicano che è pronto. Sulla CLI e sull'app, il comando si interrompe con un errore. Sul "side-car", il risultato dell'avvio rimane positivo, quindi nessun elemento elimina il pod, il piano rimane attivo e il lavoro rimane `running`. Un processo del motore che è già stato chiuso non è oggetto di questa segnalazione. Sul "side-car", si tratta di un avvio fallito e il compensatore elimina il pod. Un pod avviato con `offrig up` o dall'app non ha un "watchdog". Quando il comando si interrompe con un errore (un blocco, un motore non reattivo, un motore che è stato chiuso o un timeout), il pod continua a essere addebitato fino a quando `offrig down <profile> --yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED` non è l'eccezione: in questo caso, viene terminato su tutte le interfacce.

Il progetto e le relative prove sono disponibili in [docs/sidecar-design.md](docs/sidecar-design.md).

### Indice del progetto e strumento di verifica

```text
offrig index src docs                 chunk this project; only changed files are re-embedded
offrig index --status                 counts, model, dimension; changes nothing
offrig index --rebuild                drop every vector and embed again
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on
offrig verify calibrate --think-policy grounded=off,reasoning=escalate:off>on --model gemma4:31b gold/grounded.jsonl
offrig verify calibrate --report-only .offrig/out/calibrate-<model>-<time>
```

`offrig index` viene integrato con un Ollama solo per CPU (`embed_url`, predefinito `http://127.0.0.1:11490`), mai con il server condiviso sulla porta 11434 e mai con una GPU. Una volta che il progetto ha un indice, `offrig_memory_search` per impostazione predefinita utilizza un approccio ibrido (parola chiave più significato). `offrig verify calibrate` valuta un modello di loopback locale come strumento di verifica, una sola affermazione valida per chiamata, con le prove dell'affermazione stessa. Un nome di modello che contiene "cloud" viene rifiutato. `--num-ctx` per impostazione predefinita utilizza `auto` e definisce una finestra per l'esecuzione. `--keep-thinking` scrive il testo di ragionamento in `thinking.jsonl` solo nella directory di esecuzione. Il [riferimento del manuale](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/) elenca i flag.

### "Lane": un "side-car" per progetto, nessun conflitto

Due progetti possono eseguire "side-car" contemporaneamente su un singolo account RunPod. Ogni progetto ottiene il proprio **"lane"**: un alias SSH, una porta tunnel e un tag del nome del pod che nessun altro progetto condivide.

| | "Lane" standard (la CLI, l'app, Zed) | "Lane" di un progetto |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Porta tunnel | `11435` (runner `11436`) | prima disponibile tra `11500`, `11502`, ... (runner: la porta sopra) |
| Porta "side-car" (driver shell) | nessuna | `11700` + lo slot del "lane": `11700`, `11701`, ... |
| Nome del pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Blocco SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` deriva dal nome della cartella del progetto (`aspire-si`, `ai-jam-sessions`), con un hash breve aggiunto quando due progetti condividono lo stesso nome di cartella. Il "lane" di un progetto viene allocato la prima volta che pianifica una sessione, scritto in `lanes.toml` nella directory di configurazione di offrig e conservato: lo stesso progetto ottiene lo stesso "lane" dopo ogni riavvio. L'allocazione utilizza un file di blocco e scrive il registro in modo atomico, in modo che due "side-car" avviati contemporaneamente non condividano mai un tag, un alias o una porta. Nessun "lane" può essere `11434` (la porta dell'Ollama locale): l'intervallo inizia da `11500` e un registro modificato per indicare diversamente viene rifiutato. Un piano registra il suo "lane" e il suo avvio, il runner, il "watchdog" e l'arresto utilizzano tutti quel "lane", non la configurazione globale.

Un "side-car" corrisponde, elenca o arresta solo i pod denominati per il proprio "lane". Il "lane" di un altro progetto, i pod del "lane" standard `offrig-<profile>` e qualsiasi altro pod sull'account vengono lasciati inalterati: il controllo di avvio per un pod attivo richiede solo il proprio "lane", l'arresto rifiuta un pod il cui nome non è quello del "lane" del piano e il controllo dei pod orfani del tunnel elimina un pod obsoleto `ssh` solo quando il suo forward e il suo alias sono del "lane" stesso. I piani creati prima dell'esistenza dei "lane" non hanno un "lane" registrato e continuano a essere eseguiti sul "lane" standard, quindi un pod avviato con il vecchio schema viene chiuso dallo stesso piano che lo ha avviato.

**La porta del "side-car".** `offrig-mcp` comunica tramite MCP su stdio. Un driver shell che mantiene una connessione aperta per l'intera sessione (quando la connessione MCP della sessione stessa è interrotta) la mette dietro una porta HTTP di loopback e quella porta era un numero per l'intera macchina (`11439`): un secondo driver del progetto o qualsiasi altro programma poteva prenderla e il primo "side-car" si sarebbe spento senza dire nulla. Il valore predefinito è ora per progetto, dal "lane" del progetto, nello stesso modo in cui viene definita la porta del tunnel: lo slot del "lane" `i` (porta del tunnel `11500 + 2i`) ottiene la porta del "side-car" `11700 + i`. L'intervallo `11700` a `11763` si trova sopra ogni porta del tunnel e del runner che un "lane" può avere (`11500` a `11627`), il `11435` e il `11436` del "lane" standard e l'Ollama locale `11434`, in modo che una porta del "side-car" non possa mai essere una porta del tunnel. Non vengono memorizzate nuove informazioni: `lanes.toml` non cambia e la porta deriva dal "lane". `OFFRIG_SIDECAR_PORT` lo sovrascrive comunque; un valore che non è una porta, è inferiore a 1024 o è `11434`, `11435`, `11436` o qualsiasi valore nell'intervallo del tunnel del "lane" viene rifiutato.

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Con `--check`, una porta già in uso è un errore che indica la porta e, quando un side-car "offrig" risponde, indica il progetto a cui serve: `la porta del side-car 11700 è già in uso: un side-car "offrig" sta già servendo il progetto <path>. Prima, interrompilo oppure imposta OFFRIG_SIDECAR_PORT su una porta libera`. Il controllo verifica in che modo il driver risponde già (una richiesta come se non ci fosse alcun progetto in esecuzione, richiesta che il driver rifiuta prima di toccare qualsiasi strumento), quindi non cambia nulla in un side-car in esecuzione. `offrig_status` segnala la `sidecar_port` della corsia.

**Una corsia, un pod attivo.** Una corsia ha un alias SSH e una porta tunnel, quindi serve un solo pod alla volta: un secondo pod nella corsia (`offrig-<tag>-job` accanto a `offrig-<tag>-jam`) ripunta l'alias su se stesso e invia la `offrig_put`, `offrig_exec` e `offrig_get` del primo piano alla macchina sbagliata. `offrig_launch` quindi rifiuta finché la corsia ha un piano aperto o qualsiasi pod attivo, con `la corsia <tag> ha un pod attivo <name> (piano <id>); prima, interrompilo`, before anything is committed or rented. The plain lane's `avvia "offrig"`. L'app rifiuta allo stesso modo per un pod di un profilo diverso (il pod dello stesso profilo viene comunque riutilizzato). Interrompi il primo piano, quindi avvia il successivo.

## Livelli

I profili sono memorizzati in `%APPDATA%\offrig\config.toml` (scritti alla prima modifica). Valori predefiniti:

| Profilo | GPU | Modelli | Costo tipico |
|---|---|---|---|
| piccolo | 1 × RTX 2000 Ada / classe A4000 | `qwen3:4b` | circa 0,25 dollari/ora |
| medio | 1 × RTX PRO 6000 (96 GB); A100 o H100 80 GB se non ce ne sono di libere | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | 2,09 dollari/ora (A100 come alternativa a 1,59 dollari) |
| avanguardia | 4 × RTX PRO 6000 (384 GB), **SGLang** | Qwen3-Coder-480B AWQ 4-bit (252 GB), circa 130 GB rimanenti per il contesto | 8,36 dollari/ora |
| avanguardia-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 GB): il percorso del motore di avanguardia, testato in modo economico | circa 1,7 dollari/ora |
| avanguardia-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 GB): i kernel MoE a 4 bit dell'avanguardia, testati in modo economico | circa 1,7 dollari/ora |
| lavoro | 1 × RTX PRO 6000 (96 GB); A100 o H100 80 GB se non ce ne sono di libere | nessuno: un **pod di lavoro** esegue il tuo lavoro, non un server di modelli | 2,09 dollari/ora (A100 come alternativa a 1,59 dollari) |
| jam | 1 × A40 (48 GB) prima; A6000, A5000, 3090, L4 o 4090 se non ce ne sono di libere | nessuno: un **pod di lavoro** per i rendering di canto delle sessioni di "ai-jam" (SoulX-Singer) | 0,49 dollari/ora (A40) |

Un profilo con una `recipe` esegue un motore diverso da Ollama: un'immagine fissa (`lmsysorg/sglang:v0.5.20-cu130`), un modello Hugging Face che scarica all'avvio e argomenti server aggiuntivi. "offrig" imposta il parallelismo dei tensori in base al numero di GPU, la lunghezza del contesto in base al profilo e mantiene il motore nel loopback del pod; una ricetta non può sovrascrivere questi valori. Per un repository protetto, `hf_token_secret` indica un segreto RunPod, a cui si fa riferimento come `{{ RUNPOD_SECRET_<name> }}` in modo che il token non entri mai nelle specifiche del pod. L'avvio segnala i pesi su disco mentre il motore li scarica e si interrompe immediatamente (con il log del motore) se il motore si chiude. Non indica che il motore è pronto quando `/health` risponde e `/v1/models` che risponde non è comunque un indicatore di prontezza. Dopo che `/health` restituisce 200, "offrig" invia un completamento di chat (`max_tokens` 1, `temperature` 0) e attende fino a 30 secondi. Il passaggio è `ready` solo quando quella risposta ha una scelta. Se il completamento fallisce o scade, "offrig" riprova dopo 15 secondi. Un secondo fallimento è il passaggio `engine_unresponsive`: il lavoro continua a essere eseguito e l'errore e il log del motore sono disponibili nel lavoro. L'azione successiva è `il motore non risponde ai completamenti: interrompi il pod o attendi se è ancora in fase di riscaldamento`. "offrig" non interrompe il pod per questo, né sul side-car, né sulla CLI, né sull'app. Una prima richiesta lenta (acquisizione del grafico, JIT) non è un motivo per abbandonare il download e il caricamento; la scadenza del piano limita comunque il costo. L'orologio di attesa inizia da `grep -c` di `Application startup complete` sulla stessa lettura ssh del log, sull'intero log del motore, non solo sulle ultime 40 righe. Un motore "chiacchierone" può spostare la riga di avvio fuori da quella coda. Il conteggio avvia comunque l'orologio. Se tale conteggio è almeno uno e `/health` rimane a zero per 3 minuti, il passaggio è `engine_hung`: quelle ultime 40 righe, i secondi trascorsi da quando il conteggio è stato visto per la prima volta e l'azione successiva `shut the pod down: the engine finished startup but never became healthy`.
"offrig" segnala tale attesa e lascia il pod attivo. L'interruzione del pod dipende da te, come nel caso di una mancata corrispondenza del conteggio delle GPU. Un avviso sul conteggio delle GPU non viene aggiunto all'azione successiva o a quella non reattiva, quindi la frase rimane esatta. Un processo del motore che si è già chiuso non viene segnalato. Sul side-car, il compensatore elimina quel pod. Sulla CLI e sull'app, il comando si interrompe con l'errore e non lo elimina.

Ogni profilo elenca i tipi di GPU in ordine di priorità. La versione 2 indica un tipo e non esegue il fallback, quindi "offrig" prova l'elenco stesso, in ordine, e mai oltre il prezzo del piano. Quando nessuno è libero, un profilo può attendere (`wait_for_gpu_minutes`; avanguardia attende fino a 120 minuti): "offrig" controlla ogni minuto e crea il pod nel momento in cui le GPU diventano libere. Nulla viene affittato durante l'attesa, Ctrl+C o l'annullamento dell'avvio dell'app lo interrompono e, se il catalogo attivo non è disponibile, semplicemente riprova a crearlo ogni minuto. Le configurazioni multi-GPU di grandi dimensioni arrivano e scompaiono in pochi minuti. I prezzi sono i prezzi di listino "secure-cloud" del catalogo v2 (`GET /catalog/gpus`), per GPU, moltiplicati per il numero di GPU. Un elenco di NESSUNO non è gratuito. La pagina dei prezzi pubblici non è il prezzo disponibile. `CUDA_VERSIONS` è solo il fallback quando l'elenco `cudaVersions` di una GPU viene omesso.

### In attesa di capacità

Un piano con capacità limitata tramite `no_fallback` o `max_price_hr` spesso non riesce a soddisfare la richiesta, quindi il lancio riprova in silenzio invece di fallire, senza affittare nulla nel frattempo. Il tempo di attesa è, in ordine: il `wait_minutes` del piano (un argomento `offrig_plan`, memorizzato con il piano; `0` fallisce immediatamente), altrimenti il `wait_for_gpu_minutes` del profilo. Il profilo `job` ha un valore predefinito di 20 minuti. L'attesa viene ridotta al tempo rimanente del piano meno una riserva di cinque minuti, in modo che non superi mai la scadenza del piano e, poiché non viene affittato nulla durante l'attesa, non aggiunge nulla al peggiore dei casi previsto. `offrig_launch` segnala `capacity_wait_minutes`; durante l'attesa, `offrig_job` mostra `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) e un passaggio che indica quale controllo è in corso. Quando l'attesa termina, il lancio fallisce con `no capacity` e non viene affittato nulla.

### Definizione delle caratteristiche hardware di un piano

Un profilo elenca i tipi di GPU in ordine di priorità e RunPod seleziona la prima GPU disponibile, quindi, senza limiti, un piano potrebbe utilizzare una GPU di riserva con meno memoria, un driver più vecchio e un prezzo diverso. Tre limiti consentono di definire l'hardware che un piano può utilizzare. La pianificazione è ancora gratuita; i limiti limitano solo ciò che il piano può affittare.

| Limite | Dove | Effetto |
|---|---|---|
| `min_cuda` | profilo (`config.toml`) | La versione CUDA più vecchia dell'host, dall'elenco di RunPod (`13.2`, `13.1`, `13.0`, `12.9`, ... `11.8`). La richiesta di creazione del pod invia questo valore come `gpu.minCudaVersion`, quindi un host più recente della versione più recente copiata è comunque valido. Quando il catalogo restituisce `cudaVersions` per una GPU, un piano utilizza solo tale tipo se è disponibile una versione pari o superiore a questo valore. Quando il catalogo omette l'elenco, l'elenco copiato viene utilizzato come fallback. Per un profilo di lavoro, viene utilizzata la versione più recente tra questa e la versione `[profiles.job] min_cuda` dell'immagine. |
| `min_vram_gb` | profilo | La quantità minima totale di VRAM (tutte le GPU del profilo insieme) che un piano accetta. Le offerte inferiori vengono scartate; anche un tipo per il quale RunPod non elenca alcuna memoria viene scartato. |
| `max_price_hr` | argomento `offrig_plan` | Il costo massimo del pod, in totale $/ora per tutte le sue GPU (il valore mostrato da `offrig_offers`). Le offerte superiori vengono scartate, così come un tipo per il quale non è elencato alcun prezzo (non può essere vincolato a un limite massimo). |
| `no_fallback` | argomento `offrig_plan` | È consentita solo la prima famiglia di GPU del profilo. Le due edizioni RTX PRO 6000 Blackwell (Server e Workstation) appartengono a una famiglia; tutte le altre schede, inclusa l'A100 SXM e PCIe, appartengono a famiglie diverse. |

I campi del profilo sono facoltativi e per impostazione predefinita non sono impostati, quindi un `config.toml` scritto da un offrig precedente viene caricato senza modifiche. Il profilo `job` imposta `min_cuda = "13.0"`.

`cuda_versions` è un elenco esatto facoltativo oltre a `min_cuda`, ad esempio `cuda_versions = ["13.0"]`. Quando è impostato, la richiesta di creazione del pod invia tale elenco come `gpu.allowedCudaVersions`, nell'ordine in cui è stato scritto, e non invia anche un valore minimo. `offrig_plan` segnala l'elenco. L'impostazione insieme a `min_cuda`, inclusa la versione minima di un'immagine di lavoro, non è consentita: i due insieme sono ambigui. Ogni versione deve essere una versione che l'offrig accetta. Questo elenco, dalla versione più recente alla più vecchia, è 13.2, 13.1, 13.0, quindi 12.9 fino a 11.8. 13.2 e 13.1 sono versioni successive rispetto all'elenco v1 documentato di RunPod (letto il 2026-10-11, che si interrompe a 13.0). Sono elencate perché un pod di scrittura del 2026-10-11 è stato posizionato su un host 13.2. Un valore minimo invia comunque solo `gpu.minCudaVersion`.

`offrig_plan` calcola il peggiore dei casi in base a ciò che è rimasto: `max_hours x min(max_price_hr, the dearest listed price among the remaining GPUs)`. Senza `max_price_hr`, questo è il prezzo più alto elencato nel profilo, come prima. Un piano senza più risorse disponibili viene rifiutato con il motivo per cui ogni GPU è stata scartata e non viene scritto nulla.

Il piano memorizza l'elenco delle GPU rimanenti e, o il valore CUDA minimo o l'elenco CUDA esatto, e `offrig_launch` affitta solo da ciò che il piano ha memorizzato, mai da una modifica successiva del profilo. `offrig_job` e il risultato del lancio del side-car segnalano il tipo di GPU e la versione CUDA dell'host effettivamente affittata, in un oggetto `rented`. L'API del pod non segnala la versione CUDA dell'host, quindi, una volta che ssh è attivo, il lancio esegue `nvidia-smi` una volta attraverso il canale e la legge dall'intestazione (`CUDA Version: 12.8` o `CUDA UMD Version: 13.4` sui driver più recenti); `rented.cuda_source` indica `nvidia-smi` o `pod API`. Se la versione CUDA dell'host è precedente al valore minimo del piano, la GPU non è una di quelle elencate nel piano o il prezzo è superiore al limite del piano, `offrig_job` restituisce una voce `warnings` e avvia `next_action` con `WARNING`. Nulla viene terminato automaticamente: l'interruzione dell'affitto è a discrezione del chiamante (`offrig_shutdown`). Se né l'API del pod né `nvidia-smi` forniscono una versione CUDA, `rented.notes` lo indica e il valore minimo non viene controllato.

### Disco del contenitore

Un pod ha due dischi: il disco del contenitore, locale all'host, e il volume montato in `/workspace`. Su alcuni host, `/workspace` è un file system di rete lento: un pod di lavoro ha misurato 32 MB/s rispetto a 354 MB/s sul suo disco del contenitore e non è stato in grado di recuperare circa 130 GB di modelli in tempo, mentre il disco del contenitore era solo di 60 GB. La dimensione del disco del contenitore è `container_disk_gb` del profilo (da 30 a 60 GB nei profili predefiniti; il profilo `job` ha 60 GB) e viene inviato alla richiesta di creazione del pod come `disk`. `offrig_plan` accetta `container_disk_gb` (da 1 a 2000) per sovrascriverlo per un piano; il piano lo memorizza, il lancio lo invia e la risposta del piano e `offrig_status` mostrano la dimensione effettiva.

- Il disco del contenitore non ha un costo: offrig calcola solo il tempo di utilizzo della GPU, quindi il peggiore dei casi del piano è lo stesso con qualsiasi dimensione. RunPod addebita il costo del disco; non è stato verificato se la tariffa che segnala per il pod (`offrig_job` la mostra) include il disco del contenitore.
- offrig non sposta i download per te. I comandi di lavoro iniziano con `HF_HOME` sul volume `/workspace` (`/workspace/hf`); per utilizzare il disco del contenitore, imposta il tuo (`HF_HOME=/root/hf python ...`) nel comando.
- Il disco del contenitore viene eliminato insieme al pod, come il volume senza un volume di rete: copia i risultati con `offrig_get` prima di `offrig_shutdown`.
- Il limite da 1 a 2000 è un controllo di sanità mentale di offrig contro un errore di battitura; il limite effettivo di RunPod non viene controllato.

### Pod di lavoro

Un profilo con un `job` affitta una GPU per un lavoro che viene eseguito su di essa, come un ciclo di addestramento, anziché per servire un modello. Il suo pod esegue un'immagine PyTorch definita (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 per Blackwell) con sshd e nient'altro:

- Non serve a nessun modello, quindi non c'è tunnel e nulla è collegato a Zed. sshd non consente
nessun tipo di inoltro (`AllowTcpForwarding=no`); l'unico modo per accedervi è tramite ssh al pod.
- Un profilo di lavoro non elenca alcun modello e non può avere anche una ricetta; il controllo della configurazione rifiuta
entrambi.
- `offrig up` e l'app rifiutano un profilo di lavoro prima di allocare qualsiasi risorsa. L'esecuzione avviene
attraverso il side-car: `offrig_plan profile=job`, `offrig_launch`, quindi `offrig_put`,
`offrig_exec` e `offrig_get`. L'avvio è pronto quando sshd risponde.
- Un comando viene eseguito in modo indipendente sul pod (`setsid nohup`) in `/workspace/job`, quindi la sua durata
supera quella del side-car e della sessione ssh. Viene inviato in formato base64, quindi nulla al suo interno viene letto dalla
shell ssh. Il suo log e lo stato di uscita vengono salvati in `/workspace/offrig/jobs/`. I download di Hugging Face
vengono salvati in `/workspace/hf` sul volume del pod.
- `offrig_exec action=run` è per controlli rapidi (`ls`, `nvidia-smi`), non per il lavoro: esegue
il comando fino al completamento sotto `timeout` (predefinito 30 s, massimo 120 s) e restituisce `stdout`,
`stderr`, `exit_code` e `timed_out`. L'output viene troncato agli ultimi 64 KB di ogni flusso
(`truncated`) e rappresenta un output del pod non attendibile. Un comando che richiede più tempo è un `start`.
- La coda del log di un lavoro presenta barre di avanzamento compresse: le ridisegnature in stile tqdm, unite tramite ritorni a capo, mostrano solo l'ultimo frame. `offrig_exec action=status save_log=<local path>`
copia anche l'intero log del lavoro, così come è stato scritto, in un file locale (vengono create le cartelle principali),
in modo che la coda possa rimanere breve.
- L'immagine è una build CUDA 12.8, quindi un profilo di lavoro indica la versione CUDA più vecchia dell'host su cui
viene eseguita (`min_cuda = "12.8"`) e il pod viene creato con tale valore minimo come
`gpu.minCudaVersion`. In caso contrario, un host con un driver più vecchio avvia il pod e torch non trova alcuna GPU,
dopo che l'allocazione è iniziata. Il lavoro stesso potrebbe richiedere più risorse rispetto all'immagine: il profilo `job`
imposta anche `min_cuda = "13.0"` sul profilo (vedere sopra), perché i lavori che esegue installano una
versione corrente di vLLM, la cui PyTorch è una build CUDA 13.
- Budget, piano, watchdog e arresto funzionano come per qualsiasi altro profilo. Copia i risultati prima di `offrig_shutdown`: il disco del pod viene eliminato insieme ad esso.
- `jam` è il profilo di lavoro che ai-jam-sessions utilizza per rendere le sue esecuzioni vocali: SoulX-Singer richiede molte meno risorse di una scheda di addestramento, quindi alloca una scheda economica da 24-48 GB. La configurazione e la sessione
si trovano in tale repository (`docs/vocal-offrig.md`); offrig non sa nulla di canto.

### Caricamento dei pesi su un volume di rete

Un profilo di ricetta scarica i suoi pesi ad ogni avvio: per il modello di riferimento, ciò richiedeva circa
20 dei 22 minuti necessari per la preparazione (252 GB, $ 8,36/ora). Il caricamento li posiziona su un volume di rete RunPod una sola volta:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- Il volume viene fatturato mensilmente, indipendentemente dal fatto che un pod sia in esecuzione o meno (300 GB per il modello di riferimento costano circa
$ 21/mese a $ 0,07/GB), quindi solo un utente può eseguire il caricamento; nessun strumento automatizzato può farlo.
- Un volume si trova in un unico data center, quindi i pod del profilo vengono avviati solo lì e
le offerte e i piani hanno prezzi basati su tale posizione. Scegli uno con archiviazione di rete e le GPU del profilo; `offrig gpus` e la console di RunPod mostrano dove si trovano.
- Il download viene eseguito sul pod GPU più economico disponibile in tale data center. Il pod viene
terminato in caso di successo, fallimento o timeout.
- Il volume viene registrato nel profilo prima dell'inizio del download, quindi un caricamento non riuscito
non viene mai dimenticato; riesegui per riprendere, oppure `--remove`.
- Un avvio con caricamento preliminare esegue Hugging Face offline, solo quando il caricamento è completato (un indicatore sul volume). Un volume con caricamento parziale scarica invece il resto invece di fallire.

## Sicurezza finanziaria

- Prima di un avvio, offrig mostra la corrispondenza gratuita più economica e il tempo di esecuzione disponibile con il pod
in esecuzione. Se il tempo di esecuzione è inferiore a un'ora, rifiuta l'operazione a meno che non venga sovrascritta, perché quando il tempo di esecuzione è pari a zero,
RunPod interrompe tutti i pod sull'account, inclusi quelli che offrig non gestisce.
- L'arresto automatico termina il pod dopo 30 minuti se ogni GPU ha un utilizzo inferiore al 5% (configurabile,
oppure disattivato).
- La chiusura dell'app con un pod in esecuzione chiede se terminarlo o mantenerlo in esecuzione.
- offrig interagisce solo con i pod che ha creato: `offrig-<profile>` per la CLI e l'app,
`offrig-<tag>-<profile>` per la traccia side-car di un progetto (vedere Tracce). Un side-car non interagisce mai con i pod di un'altra traccia, con la traccia principale o con qualsiasi altro pod; questi vengono elencati,
ma non modificati.
- Per le sessioni dell'agente, il limite viene applicato prima di qualsiasi spesa: il caso peggiore di un piano (prezzo attuale × ore massime) viene preso in considerazione rispetto al budget impostato dall'utente e rifiutato se supera tale limite, e
un avvio richiede solo un ID piano, quindi un agente non può impostare il proprio prezzo.
- Ogni avvio side-car ha un watchdog che termina il pod alla scadenza del piano e il runner arresta il pod non appena la sua coda è vuota.

## Cosa cambia sulla tua macchina

| Cosa | Dove | Annulla |
|---|---|---|
| Provider Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; l'originale viene conservato come `settings.json.offrig.bak` |
| Modello predefinito di Zed (solo se richiesto) | stesso file | `offrig zed-remove` ripristina il valore predefinito precedente |
| `OFFRIG_API_KEY` (segnaposto; Zed richiede una chiave) | ambiente utente | `setx OFFRIG_API_KEY ""` oppure rimuovilo nelle proprietà di sistema |
| Alias SSH `offrig` | `~/.ssh/config`, tra i marcatori `# >>> offrig:offrig >>>` | elimina il blocco contrassegnato |
| Alias SSH `offrig-<tag>`, uno per progetto che è stato avviato da un side-car | `~/.ssh/config`, tra i marcatori `# >>> offrig:offrig-<tag> >>>` | elimina il blocco contrassegnato |
| Tracce del progetto | `%APPDATA%\offrig\lanes.toml` (percorso del progetto, tag, alias, porta del tunnel) | elimina la voce del progetto mentre nessun pod è in esecuzione nella sua traccia, oppure l'intero file |
| Chiavi host del pod | `~/.ssh/known_hosts_offrig` | elimina il file |
| Impostazioni | `%APPDATA%\offrig\config.toml` | elimina il file |
| Pesi caricati in precedenza (solo con `offrig stage --yes`) | un volume di rete RunPod `offrig-<profile>`; fatturato mensilmente | `offrig stage <profile> --remove --yes` |

Commenti e layout nelle impostazioni di Zed vengono conservati: le modifiche vengono elaborate tramite un albero di sintassi JSONC.

## Modello di minaccia

- **RunPod API key.** Read from `RUNPOD_API_KEY`; never written to disk or logs. The Zed
  provider is deliberately not named `runpod`, because Zed would then read
  `RUNPOD_API_KEY` and send it to the model server.
- **Model server.** Reachable only through SSH with your key. Password login is off on
  the pod, and sshd allows only local forwarding.
- **Host keys.** Pinned per endpoint in a separate known-hosts file. offrig forgets a key
  only when the pod endpoint changes, because RunPod reuses ip:port pairs across pods.
- **Shell injection.** Model names are validated against Ollama's name syntax before
  they reach a remote shell.
- **Orphaned tunnels.** If offrig dies, its `ssh` may keep holding the port. On the next
  start offrig kills it, but only if the listener is `ssh.exe` carrying offrig's exact
  forward spec. Anything else on the port is refused, never killed.
- **No telemetry.** offrig talks only to RunPod's API, your pod, and your local Ollama
  (to compare model lists).

## Test

`cargo test --workspace` esegue più di 250 test, coprendo almeno il 90% delle righe (il CI fallisce se questo valore è inferiore):

- **La libreria principale:** analisi di RunPod, specifiche del pod per entrambi i motori, configurazione SSH, modifiche JSONC di Zed, regole di protezione, logica di costo e inattività, lo store e le sue migrazioni, ruoli, assemblaggio del contesto, controlli deterministici, le decisioni del runner, il watchdog e lo staging, inclusa un'istanza di RunPod simulata che dimostra che una fase fallita interrompe il suo pod.
- **L'applicazione:** gestione dello stato più test dell'interfaccia utente tramite clic nell'ambiente di test di egui.
- **La CLI:** codici di uscita, livelli di log e il fatto che la chiave API non compaia mai nell'output.
- **Il side-car:** test end-to-end tramite stdio rispetto a un'istanza di RunPod simulata, il processo reale del watchdog e il processo reale del runner rispetto a un modello di pod simulato; ogni errore dello strumento restituisce un codice.

`scripts/verify.sh` (o `scripts/verify.ps1`) esegue il controllo del formato, clippy, i test e un'esecuzione di prova di ogni eseguibile in un unico comando. Il CI esegue anche `cargo deny`, una scansione OSV di `Cargo.lock`, la copertura su Codecov e `atlas check`.

### Registrazione dei test in tempo reale (02-10-2026, livello medio, A100 80 GB, circa 0,45 dollari)

- Il pod si avvia in circa 80 secondi; sshd, il tunnel e l'istanza Ollama 0.35.0 del pod rispondono.
- 97 GB di modelli vengono scaricati sul pod a una velocità di circa 150-250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` e `gpt-oss:120b` rispondono entrambi a una chat in streaming con una corretta chiamata allo strumento tramite il tunnel. Hanno utilizzato 36 GB e 64 GB della VRAM del pod; l'istanza locale di Ollama non ha caricato nulla e non era presente sulla GPU locale.
- Tutti e sette i controlli di protezione sono stati superati, sia dalla CLI che dall'applicazione.
- Una CLI interrotta bruscamente ha lasciato il suo `ssh` in esecuzione sulla porta; l'esecuzione successiva l'ha recuperata.
- Il tunnel, i controlli, il test del modello e l'arresto dell'applicazione sono stati eseguiti tramite i suoi pulsanti.

Bug riscontrati durante l'esecuzione in tempo reale, ora corretti e coperti: una lista `&&` in background manteneva stdout di ssh aperto e bloccava l'avvio del download; la lista dei pod non includeva i tipi di GPU senza `includeMachine=true`; il controllo di avvio contava due volte il prezzo di un pod in esecuzione.

### Esecuzione di prova del side-car (03-10-2026, livello piccolo, RTX 2000 Ada, 0,08 dollari prenotati)

L'istanza `offrig-mcp` installata viene eseguita tramite stdio, nel modo in cui un agente la chiama:

- `offrig_plan` ha calcolato un prezzo di 0,5 ore a 0,15 dollari nel caso peggiore; `offrig_launch` lo ha confermato, ha avviato il watchdog e una seconda chiamata ha restituito lo stesso lavoro. È stato affittato un pod, al costo di 0,24 dollari/ora.
- SSH è attivo 100 secondi dopo l'affitto, `qwen3:4b` è stato scaricato ed è pronto in 150 secondi.
- `offrig_ask` ha eseguito un passaggio di un game designer in 46 secondi; la risposta ha soddisfatto il controllo di accettazione e ha mantenuto la restrizione di cinque elementi dalla memoria.
- Il pod ha raggiunto Internet (Wikipedia, API di GitHub). In base ai dati forniti, il pod ha risposto correttamente alle domande correnti; se interrogato a freddo, ha affermato di non avere accesso in tempo reale.
- `offrig_shutdown` da un processo side-car appena avviato ha terminato il pod e ha chiuso i conti; il watchdog ha rilevato la chiusura del piano ed è uscito. La GPU locale è rimasta inattiva durante tutto il processo.

Risolti i seguenti problemi: il pod gestiva una richiesta alla volta (`OLLAMA_NUM_PARALLEL=1`); quattro slot hanno gestito 8 richieste parallele da 40 a 102 token/s sulla stessa GPU, quindi ora ogni profilo ha `parallel = 4`. `complete` è stato rifiutato senza una motivazione (ora per impostazione predefinita viene visualizzato "controllo di accettazione superato"; i fallimenti devono comunque indicarne uno). Lo stato suggeriva di registrare brevemente mentre una sessione era attiva. Si pensa che il testo che trapela in una risposta venga eliminato e che una risposta svuotata dal processo di "pensiero" indichi di aumentare `max_tokens`.

### Esecuzione di prova del runner (03-10-2026, livello piccolo, RTX 2000 Ada, 0,04 dollari prenotati)

Cinque passaggi, uno dipendente dall'altro, sono stati eseguiti da `offrig_run` senza che nessuno li guidasse:

- Quattro passaggi in esecuzione contemporaneamente su quattro slot (12,9 GB di 16 GB di VRAM); il passaggio dipendente è iniziato nel momento in cui è terminato il passaggio da cui dipendeva e ha utilizzato il suo risultato.
- I tre passaggi i cui controlli coprivano l'accettazione sono stati completati autonomamente; le storie rivali (controlli parziali) e la storia (nessun controllo) sono state inviate per la revisione.
- La revisione ha restituito la storia ("il fiume prende il nome dal progetto"); il runner attivo l'ha adottata e l'ha rivista in base al feedback ("fiume Veyl").
- La coda è stata svuotata in 6,5 minuti (6 turni, 20.861 token); il runner ha interrotto il pod.

Appreso: i controlli deterministici verificano la struttura, non la qualità del design. Il modello da 4B ha superato "tre verbi" con verbi deboli, quindi `accept_on_checks` è per il lavoro strutturale e il lavoro di design viene inviato per la revisione. qwen3:4b ha impiegato circa 4.000 token per "pensare" a ogni turno, anche con solo tre righe di storia. Una coda mantiene ogni slot occupato solo quando contiene un numero sufficiente di passaggi indipendenti; una catena di dipendenze viene eseguita uno alla volta.

### Esecuzioni di Frontier e SGLang (03-10-2026, 4,27 dollari prenotati)

| Esegui | Pod | Pronto dopo | Coda | Prenotato |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 minuti | 4 passaggi in 15 secondi | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 minuti | 4 passaggi | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 minuti** (252 GB a 278 MB/s, quindi caricamento) | **31 passaggi in 20 secondi** | **$3.59** |
| 30B, esecuzione di test su un insieme di modelli | 1 × RTX PRO 6000 | 10 minuti (posizionamento lento dei pod) | solo esecuzione di test | $0.43 |

- SGLang v0.5.20 (cu130) in esecuzione su Blackwell: attenzione flashinfer, `awq_marlin` per i
pesi MoE a 4 bit, parallelismo tensoriale su PCIe su quattro schede; `/dev/shm` era 352 GB.
- Il lavoro all'avanguardia è chiaramente migliore rispetto ai modelli più piccoli: risposte nel contesto, e un
modulo Rust che è stato compilato e ha superato i suoi tre test (verificati localmente). La versione a 30B FP8
della stessa attività non è stata compilata.
- Un modello caricato serve un intero insieme di modelli; non sono necessarie copie. Esecuzione di test di concorrenza con
risposte di 384 token, token totali al secondo:

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

La velocità per agente diminuisce all'aumentare del numero di agenti (480B: 88 → 24 token/s a 64), ma la
capacità di elaborazione totale continua ad aumentare; i miglioramenti del 480B si stabilizzano dopo 256. La cache KV all'avanguardia
contiene 398.526 token, quindi con contesti di passaggio reali di 2-8k token, il livello all'avanguardia
ora esegue 64 operazioni contemporaneamente e i livelli SGLang a singola scheda 32.

Durante il processo, sono stati trovati e corretti i seguenti problemi: le revisioni hanno aggiunto un riempimento all'output per superare un controllo dell'intestazione (ora un
controllo `no_repeats` integrato, e le revisioni ristrutturano sul posto); l'output richiesto di un ruolo è stato divulgato nei risultati (il passaggio ora imposta il formato); il feedback dell'intestazione
indica il formato Markdown; una revisione invariata si interrompe invece di ripetere.

Non ancora verificato in tempo reale: una chat
inviata direttamente dal pannello dell'agente di Zed (la forma della richiesta utilizzata da Zed viene testata direttamente) e il controllo di disponibilità 1.1.0, il rapporto sugli errori e il pin CUDA esatto su un pod in affitto (vedere
[Stato](#stato)).

## Conformità agli standard

Valutato in base agli standard del flusso di lavoro dello studio (0 mancanti, 1 parziali, 2 presenti,
3 esemplari).

- **PIN_PER_STEP: 2.** Le immagini dei pod sono associate a tag di versione (`ollama/ollama:0.35.0`,
`lmsysorg/sglang:v0.5.20-cu130`; una ricetta rifiuta `latest`), il compilatore alla versione 1.98.1,
le dipendenze tramite `Cargo.lock` e il motore Atlas alla versione 1.24.0 della flotta. Ogni passaggio
registra il modello, l'hash del ruolo e l'hash del prompt. I modelli sono associati tramite tag o ID del repository,
non tramite hash.
- **ANDON_AUTHORITY: 3.** Ogni passaggio interrompe l'esecuzione in caso di difetto: un piano i cui pesi
superano lo spazio su disco viene rifiutato prima di qualsiasi spesa; un pull non riuscito interrompe l'avvio; una modifica di Zed
che non viene riletta non viene scritta; un file di impostazioni danneggiato viene segnalato, ma non
riscritto; CI blocca su fmt, clippy, test, licenze e avvisi.
- **NAMED_COMPENSATORS: 2.** Ogni azione irreversibile ha un'azione di annullamento, elencata di seguito.
- **DECOMPOSE_BY_SECRETS: 2.** Un modulo per ogni elemento che cambia per le proprie ragioni:
l'API di RunPod (`runpod`), il contenuto del pod (`spec`), il trasporto (`tunnel`,
`remote`), ogni file locale modificato da offrig (`sshconfig`, `zed`) e le regole (`guard`,
`cost`). Le interfacce utente non contengono alcuna logica oltre alla presentazione.
- **UNCERTAINTY_GATED_HUMANS: 2.** offrig chiede solo quando il risultato è costoso o
causa perdite: avvio entro un'ora di tempo disponibile, terminazione di un pod (con indicazione di cosa viene perso) e
uscita con un pod ancora attivo. Due decisioni spettano esclusivamente a un essere umano e nessun
strumento dell'agente può prenderle: il limite di budget e la preparazione di un volume, che addebita mensilmente.
L'output del passaggio che il codice non può controllare attende la revisione invece di essere completato.
- **EXTERNAL_VERIFIER: n/a.** Nessuna richiesta specializzata.

**Compensatori**

| Azione | Annulla | Stato dopo l'annullamento | Proprietario |
|---|---|---|---|
| Crea un pod (avvia la fatturazione) | `offrig down <profile> --yes`, l'app si spegne o si arresta automaticamente | pod terminato, fatturazione interrotta | l'operatore che esegue offrig |
| Termina un pod | nessuno per il suo disco; riavvia il profilo e i modelli vengono riscaricati (un volume di rete li conserva) | nuovo pod, stesso profilo | l'operatore |
| Scrive il provider Zed o il modello predefinito | `offrig zed-remove`, o ripristina `settings.json.offrig.bak` | Zed come prima, offrig | l'operatore |
| Imposta `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` o lo elimina nelle proprietà di sistema | variabile eliminata | l'operatore |
| Scrive l'alias SSH | elimina il blocco contrassegnato in `~/.ssh/config` | configurazione come prima | l'operatore |
| Alloca una corsia di progetto (la prima `offrig_plan` del progetto) | elimina la voce del progetto da `lanes.toml` una volta che nessun pod è in esecuzione nella sua corsia; un nuovo piano alloca di nuovo | corsia libera per il riutilizzo; il blocco dell'alias è separato (riga sopra) | l'operatore |
| Scrive un blocco di alias SSH per una corsia (avvio side-car) | `offrig_shutdown` lo rimuove quando nomina il pod del piano (anche dopo un avvio non riuscito); altrimenti, elimina il blocco `# >>> offrig:offrig-<tag> >>>` in `~/.ssh/config` | configurazione come prima; i blocchi di altre corsie non vengono toccati | l'operatore |
| Scarica un modello sul pod | `ollama rm <model>` sul pod, o termina il pod | modello eliminato | l'operatore |
| Elimina un tunnel offrig orfano | nessuno necessario; solo un `ssh` con l'alias e il forward esatti di questa corsia viene eliminato, mai un'altra corsia | porta libera | offrig |
| Avvio side-car (`offrig_launch`) | `offrig_shutdown`; automatico se la configurazione fallisce; il watchdog alla scadenza; l'esecutore quando la sua coda si svuota | pod terminato, piano chiuso con spesa misurata | l'agente chiamante, con il watchdog come backup |
| Prepara un volume (`offrig stage --yes`, addebito mensile) | `offrig stage <profile> --remove --yes` | volume eliminato, profilo torna al download | l'utente che lo ha preparato |
| Avvia un lavoro su un pod di lavoro (`offrig_exec action=start`) | `offrig_exec action=stop`, o `offrig_shutdown` | lavoro interrotto con tutto ciò che ha avviato; il suo log rimane fino alla scomparsa del pod | l'agente chiamante |
| Esegue un comando breve su un pod di lavoro (`offrig_exec action=run`) | solo ciò che il comando stesso esegue; termina al massimo dopo 120 secondi (`timeout`) o con `offrig_shutdown` | il pod nello stato in cui è stato lasciato dal comando | l'agente chiamante |
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
docs/                the side-car's design and its research grounding
atlas/               Atlas map of the repo (regenerate with `atlas map`)
```

## Licenza

MIT. Consultare [LICENSE](LICENSE).

---

Realizzato da <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>
