<p align="center">
  <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.hi.md">हिन्दी</a> | <a href="README.it.md">Italiano</a> | <a href="README.md">English</a>
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

[![Pontuação M8ven](https://m8ven.ai/badge/mcp/mcp-tool-shop-org-offrig-790exm?v=3bd60e98b65d7124b74c4126c5ce5ce7)](https://m8ven.ai/mcp/mcp-tool-shop-org-offrig-790exm?s=readme)

Execute modelos grandes em GPUs RunPod alugadas, com uma garantia: eles nunca serão executados na sua própria GPU. Um aplicativo para desktop, uma CLI e um módulo auxiliar para agentes, tudo em uma única biblioteca Rust.

O side-car permite que um agente planeje uma sessão paga dentro de um orçamento definido por um humano, alugue as GPUs e envie uma fila de tarefas com papéis definidos para um executor independente. O executor mantém todos os slots de modelo em uso, revisa apenas as verificações que falharam e desliga o pod quando a fila estiver vazia. Um "watchdog" encerra o pod no prazo do plano, mesmo que tudo o mais tenha desaparecido.

## Status

Testado em 2026-10-02 e 2026-10-03, com um custo total de cerca de 5 dólares:

- **Frontier:** 4 × RTX PRO 6000 (384 GB) executando Qwen3-Coder-480B (AWQ de 4 bits) em SGLang, pronto em 22 minutos, seguido por 31 transferências em 20 segundos, com um custo de US$ 3,59.
- **Dados do Swarm:** um modelo carregado atende a centenas de agentes simultaneamente. O modelo de 480B atingiu 4.059 tokens/s com 512 agentes; um modelo de 30B em uma única placa atingiu 10.147 tokens/s com 256.
- **Executor:** uma fila com dependências e reenvios de revisão, funcionou sem ninguém controlando, o pod foi desligado quando a fila ficou vazia.
- **Garantia:** a GPU local permaneceu inativa durante todas as execuções.

Em uso diário desde 2026-10-07 por dois projetos simultaneamente, cada um em sua própria "lane": execuções de treinamento para aspire-si em `job` pods e renderizações de "singing" para ai-jam-sessions em `jam` pods.

Em 2026-10-11, um pod SGLang registrou `Application startup complete`, então `/health` permaneceu com status 503 por 7 minutos. As GPUs operavam perto de 100% com cerca de 100 W, e `/v1/models` retornou 200. Os pods em execução naquela semana usavam CUDA 13.0. Esse pod foi colocado no CUDA 13.2. Não foi demonstrado que 13.2 seja a causa; o "pin" permite que um perfil permaneça na versão que funcionou. Esta versão relata essa falha e não considera um "recipe" pronto até que uma conclusão retorne uma escolha. Um perfil também pode fixar a versão do CUDA do host.

Não foi testado em um dispositivo alugado para esta versão: a verificação de prontidão com um único token, o
os relatórios `engine_unresponsive` e `engine_hung`, e um comando que envia
`gpu.allowedCudaVersions`. Esses caminhos são cobertos por testes realizados contra uma sessão SSH simulada.
e um RunPod falso. Nenhum pod foi iniciado para verificar novamente. O v1 documentado do RunPod.
`POST /pods` enum, lido em 11 de outubro de 2026, ainda para em 13.0. As versões 13.1 e 13.2 estão em offrig.
lista porque esse podcast de escrita surgiu em 13 de fevereiro. Um andar ainda envia apenas...
`gpu.minCudaVersion`.

Construído e testado, aguardando uma decisão: preparação dos pesos do frontier em um volume de rede, cerca de US$ 21 por mês (veja [Preparação](#pesos-em-fase-de-preparação-em-um-volume-de-rede)).

Próximo: a primeira fila de tarefas real, planejada integralmente antes do lançamento; transferências de código compiladas e testadas no pod.

## O que ele faz

De uma única janela (ou comando), o "offrig" faz o seguinte:

1. mostra seu saldo RunPod, os preços atuais das GPUs e por quanto tempo o saldo durará;
2. inicia um pod para uma camada, de 1 placa pequena no Ollama até 4 × RTX PRO 6000 no SGLang, e carrega seus modelos no pod;
3. abre um túnel SSH para o pod;
4. adiciona os modelos do pod ao Zed como seu próprio provedor;
5. executa sete verificações para garantir que os modelos não sejam executados nesta máquina;
6. desliga o pod ou o encerra após um período com todas as GPUs inativas.

Através do módulo auxiliar, um agente também planeja sessões com base em um orçamento, mantém a memória do projeto entre compactações e reinicializações e executa filas de transferência sem supervisão (veja [O módulo auxiliar](#o-side-car-para-agentes)).

## A garantia e como ela se mantém

- **O servidor de modelo é inacessível, exceto através do túnel.** O pod executa um mecanismo fixo, Ollama (`ollama/ollama:0.35.0`) ou SGLang (`lmsysorg/sglang:v0.5.20-cu130`), vinculado ao próprio loopback do pod, e o pod expõe apenas `22/tcp`. Não há nenhum ponto de extremidade HTTP público para encontrar ou abusar. Um "recipe" não pode mover o mecanismo para fora do loopback.
- **Zed se comunica com o túnel, em sua própria porta.** O túnel escuta na porta `127.0.0.1:11435`. Seu Ollama local está na porta `11434`. Offrig se recusa a colocar o túnel na porta `11434`, para que um túnel inativo não possa ser redirecionado para o servidor local: a solicitação falha em vez disso.
- **Zed nunca alterna provedores.** Os modelos do pod são um provedor `offrig` separado no Zed. Se o pod estiver inativo, a escolha de um deles resultará em um erro; Zed não tentará outro provedor.
- **Os pesos nunca existem localmente.** Os modelos são carregados no pod, pelo pod (ou baixados de lá do Hugging Face, ou lidos de um volume de rede preparado).

O "guard" verifica isso a cada vez, com base em fatos que o "offrig" pode observar:

| Verificação | Falha quando |
|---|---|
| O "tunnel" evita a porta local do Ollama | a porta do "tunnel" é 11434 |
| O Zed envia os modelos do pod através do "tunnel" | A URL do provedor do Zed é qualquer coisa, menos o "tunnel" |
| O Ollama do pod não está exposto à internet | o pod mapeia a porta 11434 publicamente |
| O "tunnel" termina no pod | a lista de modelos através do "tunnel" difere da lista lida no pod via SSH |
| Os modelos do pod não estão nesta máquina | um modelo de pod também existe no Ollama local |
| Nenhum modelo de pod compartilha um nome com um modelo Zed local | um nome no provedor "offrig" também está na lista do Ollama local do Zed |
| Todos os modelos que o Zed oferece estão no pod | O Zed oferece um modelo que o pod não tem |

## Instalação

Requer Windows com OpenSSH (integrado), Zed se você quiser os modelos em um editor e uma conta RunPod.

1. Faça o download de `offrig-<version>-windows-x64.zip` em
[Releases](https://github.com/mcp-tool-shop-org/offrig/releases), verifique-o em relação ao
`SHA256SUMS` da versão e descompacte-o em seu `PATH`. Ele contém `offrig.exe` (a CLI),
`offrig-app.exe` (o aplicativo) e `offrig-mcp.exe` (o side-car).
Para construir a partir do código-fonte: `cargo build --release`, com Rust 1.98.1 (fixado em
`rust-toolchain.toml`).
2. Coloque sua chave de API RunPod na variável de ambiente do usuário `RUNPOD_API_KEY`.
3. Adicione sua chave pública SSH nas configurações da conta em RunPod. O offrig usa
`~/.ssh/runpod_rustline`, se presente, e depois `~/.ssh/id_ed25519`.
4. Para agentes, registre o side-car no Claude Code no escopo do usuário:
`claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Ele abre o
repositório de um projeto apenas no primeiro uso, portanto, é inofensivo em projetos que nunca o utilizam.

O [manual](https://mcp-tool-shop-org.github.io/offrig/handbook/) explica o
primeiro pod, o side-car, a configuração, os canais e os pods de trabalho.

## Uso

**Aplicativo:** inicie `offrig-app`, escolha um perfil e pressione **Iniciar pod**. Quando estiver pronto, os
modelos aparecerão no painel de agentes do Zed como "RunPod · …". Reinicie o Zed uma vez após o primeiro
lançamento para que ele reconheça `OFFRIG_API_KEY`.

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

`offrig --help` e `offrig <command> --help` listam todos os comandos e opções. O bloco
acima é o conjunto diário. `offrig init`, `offrig models`, `offrig zed` e
`offrig up --no-zed` estão no [manual](https://mcp-tool-shop-org.github.io/offrig/handbook/usage/).

### Saída, códigos de saída e erros

- **Níveis de registro:** `-q` imprime apenas erros e os resultados de um comando; `-v` adiciona cada
chamada RunPod e seu tempo de execução; `--debug` adiciona corpos de resposta com falha e cadeias de erros completas.
A chave da API é removida em todos os níveis.
- **Códigos de saída:** `0` sucesso, `1` algo para corrigir em seu lado (argumentos, configuração, uma
restrição ou recusa de orçamento, uma chave ausente), `2` uma falha em tempo de execução (RunPod, rede, ssh,
tempo limite, sem capacidade).
- **Erros do side-car** são resultados, nunca erros de protocolo: `ok:false` com um `code` estável,
o texto `error`, um `next_action` e `retryable`. Os códigos estão listados no
[manual de referência](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## O side-car (para agentes)

`offrig-mcp` é um servidor MCP que um agente, como o Claude Code, usa como um instrumento. Ele mantém um banco de dados por projeto em `<project>/.offrig/offrig.db` que sobrevive a todos os pods, para que uma sessão sobreviva à compactação ou a uma reinicialização sem precisar explicar tudo novamente.

| Ferramenta | O que ele faz |
|---|---|
| `offrig_status` | O projeto, o orçamento, o saldo RunPod e o tempo de execução, os pods do offrig, cada plano aberto com seu `plan_id`, canal e nome do pod, a fila de transferência com trabalhos antigos marcados. Cada pod inclui `gpu_count` e `gpus` (`2x <type>`) do registro RunPod, além do `planned_gpu_count` do plano. |
| `offrig_offers` | Ofertas de GPU em tempo real para uma contagem de GPUs |
| `offrig_plan` | Calcula o preço de uma sessão no pior cenário (preço atual x (horas máximas + 780 s de tempo limite de auto-desligamento), arredondado para centavos); recusado se exceder o orçamento restante. A resposta indica o `ssh_alias` do canal e o `pod_name` que o lançamento criará, e o `container_disk_gb` que solicitará (a opção `container_disk_gb` substitui o perfil; consulte "Disco do contêiner"). As opções `max_price_hr` e `no_fallback` restringem quais GPUs ele pode alugar (consulte "Fixação do hardware de um plano"); a opção `wait_minutes` define por quanto tempo o lançamento tenta novamente quando não há capacidade (consulte "Aguardando capacidade"). |
| `offrig_memory_search` | Pesquisa a memória ativa do projeto, cada resultado com fonte e data |
| `offrig_memory_record` | Adiciona um resumo, restrição, decisão, fato ou ponto de verificação; as alterações são substituições com um motivo |
| `offrig_handoffs` | Enfileira transferências com papéis definidos (cada uma precisa de uma verificação de aceitação; verificações determinísticas opcionais), lista-as, visualiza blocos de papéis, mostra a melhor saída de uma transferência (também gravada em `.offrig/out/`), registra os resultados (completo, inválido, violação, falha, tentar novamente com feedback) |
| `offrig_launch` | **Gasta.** Aceita apenas um `plan_id`: compromete o pior caso, aguarda o aluguel das GPUs, inicia o pod, abre o "tunnel", importa os modelos, inicia o "watchdog". É idempotente por plano. Recusado enquanto a "lane" já tiver um plano ou pod ativo: `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | Progresso do lançamento (enquanto o pod é iniciado, o passo é derivado do estado do pod no momento da chamada, e `progress.pod_status` é o status bruto v2; cada tentativa de capacidade é contada em `progress.capacity_wait`), o tipo de GPU, `gpu_count`, `gpus`, `planned_gpu_count` e, quando o ssh estiver ativo, `measured_gpu_count` (uma leitura sem cartões é `0`). `gpu_mismatch` é verdadeiro quando o registro ou a medição diferem do plano. A versão CUDA do host é medida com `nvidia-smi`, com uma entrada alta `warnings` quando o host é mais antigo do que o limite CUDA do plano. Para uma receita, `ready` significa que uma conclusão retornou uma escolha. `engine_hung` e `engine_unresponsive` mantêm o pod ativo. Tempo de vida do watchdog, minutos restantes, gasto até o momento |
| `offrig_ask` | Uma iteração de uma transferência no modelo do pod, com o contexto construído a partir do armazenamento do projeto; a resposta é retornada como saída não confiável |
| `offrig_run` | Inicia um executor independente que mantém todos os slots de modelo em uso: elabora cada transferência pronta, revisa no máximo duas vezes em relação às verificações que falharam, alimenta os resultados para as transferências dependentes e, em seguida, desliga o pod quando a fila estiver vazia (a menos que `keep_pod`). O trabalho que o código não pode verificar aguarda em revisão |
| `offrig_put` | Copia um arquivo ou diretório local para um pod de trabalho (scp); os caminhos relativos do pod estão em `/workspace/job`. `plan_id` opcional (veja abaixo) |
| `offrig_exec` | Executa um comando bash em um pod de trabalho, de forma independente, para que ele sobreviva ao side-car (`start`), relata se está em execução ou se foi encerrado, juntamente com seu código de saída e o final do log (`status`; `save_log` também copia todo o log para um arquivo local), o encerra (`stop`) ou executa um comando curto agora e retorna sua saída padrão, erro padrão e código de saída (`run`, `timeout_secs` padrão de 30, no máximo 120). `plan_id` opcional (veja abaixo) |
| `offrig_get` | Copia um arquivo ou diretório de volta de um pod de trabalho, criando pastas pai locais ausentes; faça isso antes do desligamento, que exclui o disco do pod. `plan_id` opcional (veja abaixo) |
| `offrig_shutdown` | **Destrói o pod.** Encerra-o e fecha os livros do plano com os gastos medidos; recusado enquanto as transferências estiverem em andamento, a menos que seja fornecido um motivo. Remove o bloco `~/.ssh/config` da "lane" quando ele nomeia esse pod (`ssh_block_removed`) |
| `offrig_complete` | **Gasta.** Uma conclusão de chat OpenRouter, apenas para um projeto e modelo permitidos. Compromete o pior cenário em relação ao orçamento, grava a resposta em um arquivo de projeto (substituindo esse caminho, se já existir) e registra o custo real. |

**Qual plano de trabalho uma ferramenta de trabalho executa.** `offrig_put`, `offrig_exec` e `offrig_get` aceitam um `plan_id` opcional. Com exatamente um plano de trabalho aberto e nenhum `plan_id`, eles o usam, como antes. Com mais de um plano de trabalho aberto e nenhum `plan_id`, eles se recusam e listam os planos abertos (id, perfil, nome do pod): eles nunca adivinham. Com um `plan_id`, eles agem apenas no pod desse plano e apenas depois de verificar se o nome do pod é o que o plano possui (um pod de outra "lane" é recusado, não apenas de outra "lane"). Cada resposta da ferramenta de trabalho e `offrig_job` indica o `project` e o `plan_id` em que agiu (as respostas da ferramenta de trabalho também indicam o `lane`); `offrig_status` indica o `project` e lista todos os planos abertos com seu `plan_id`, "lane" e nome do pod.

**Tarefas e skills.** Um cliente compatível com MCP Tasks pode executar `offrig_launch` como tarefa: `tasks/get` mostra o mesmo estado que `offrig_job`, e `tasks/cancel` cancela uma inicialização ainda em andamento (por meio de `offrig_shutdown`); para parar uma sessão já pronta, chame `offrig_shutdown`. O prazo do plano e o watchdog continuam decidindo quando um pod para. O servidor também publica uma skill de procedimento, `offrig-session`. Os detalhes estão no [manual do side-car](https://mcp-tool-shop-org.github.io/offrig/handbook/side-car/#tasks).

Os papéis vêm do Role OS (dossiers e cartas de "starter-pack") mais quatro papéis de jogo enviados aqui nos formatos do Role OS: game-designer, systems-designer, narrative-designer, lore-keeper. O limite do orçamento é definido apenas por um humano:

```text
offrig budget --provider runpod 30       RunPod's cap (run in the project directory)
offrig budget --provider openrouter 8    OpenRouter's cap
offrig budget --show                     every cap, beside each provider's own balance
```

A cada lançamento de side-car, inicia-se um **watchdog**: um processo separado que termina o pod no prazo definido no plano (tempo alocado + horas máximas), mesmo que o agente, a sessão ou o side-car tenham terminado. Ele nunca age em uma pesquisa malsucedida, termina exatamente uma vez, fecha os registros e registra em `.offrig/watchdog-<plan>.log`. Se a preparação de um pod alugado falhar antes que o motor esteja em execução, o side-car o termina em vez de deixá-lo em estado de cobrança. `engine_hung` e `engine_unresponsive` são apenas para fins de relatório em todas as interfaces: o side-car, a CLI e o aplicativo mantêm o pod ativo e não exibem a mensagem de "pronto". Na CLI e no aplicativo, o comando é interrompido com um erro. No side-car, o resultado do lançamento permanece com o status de sucesso, portanto, nada exclui o pod, o plano permanece alocado e o trabalho permanece `running`. Um processo de motor que já foi encerrado não é esse relatório. No side-car, é um lançamento malsucedido, e o compensador exclui o pod. Um pod iniciado com `offrig up` ou pelo aplicativo não tem watchdog. Quando o comando é interrompido com um erro (travamento, motor não responsivo, motor que foi encerrado ou tempo limite), o pod continua em estado de cobrança até que `offrig down <profile> --yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED` seja a exceção: nesse caso, ele é encerrado em todas as interfaces.

O design e suas evidências estão em [docs/sidecar-design.md](docs/sidecar-design.md).

### Índice do projeto e o verificador

```text
offrig index src docs                 chunk this project; only changed files are re-embedded
offrig index --status                 counts, model, dimension; changes nothing
offrig index --rebuild                drop every vector and embed again
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on
offrig verify calibrate --think-policy grounded=off,reasoning=escalate:off>on --model gemma4:31b gold/grounded.jsonl
offrig verify calibrate --report-only .offrig/out/calibrate-<model>-<time>
```

`offrig index` é incorporado com **EmbeddingGemma 2** pelo Google DeepMind (Apache-2.0, não restrito),
executado na CPU por [`tools/embed-serve`](tools/embed-serve/) (`embed_url`, padrão
`http://127.0.0.1:11490`), nunca no servidor compartilhado na porta 11434 e nunca em uma GPU. Ele substituiu
nomic-embed-text na versão 1.2.0 após um benchmark pré-registrado nas consultas do próprio estúdio
(vetor denso nDCG@10 +0,112); um índice criado pelo nomic é recusado até `offrig index --rebuild`. Uma vez que
o projeto tenha um índice, `offrig_memory_search` assume o padrão híbrido (palavra-chave mais significado).
`offrig verify calibrate` avalia um modelo de loopback local como um verificador, uma afirmação de ouro
por chamada, com a própria evidência dessa afirmação. Um nome de modelo que contenha "cloud" é recusado.
`--num-ctx` assume o padrão `auto` e define o tamanho de uma janela para a execução. `--keep-thinking`
escreve o texto de raciocínio em `thinking.jsonl` apenas no diretório de execução. `--jobs <N>`
(de 1 a 8, padrão 1) mantém até N afirmações em andamento e ainda as confirma na
ordem dos arquivos: o servidor deve permitir N em paralelo (`OLLAMA_NUM_PARALLEL`, que assume o padrão
de 1 no Ollama) ou solicitações extras ficam em fila, a memória KV necessária aumenta com os slots paralelos e a
concorrência pode alterar os resultados até que sejam medidos. A
[referência do manual](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/)
lista as flags.

### Canais: um side-car por projeto, sem conflitos

Dois projetos podem executar side-cars simultaneamente em uma conta RunPod. Cada projeto recebe sua própria
**fila**: um alias SSH, uma porta de túnel e uma tag de nome de pod que nenhum outro projeto compartilha.

| | Canal padrão (a CLI, o aplicativo, Zed) | Canal de um projeto |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Porta do túnel | `11435` (executor `11436`) | primeiro canal livre de `11500`, `11502`, ... (executor: a porta acima) |
| Porta do side-car (driver de shell) | nenhum | `11700` + o slot do canal: `11700`, `11701`, ... |
| Nome do pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Bloco SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` é derivado do nome da pasta do projeto (`aspire-si`, `ai-jam-sessions`), com um hash curto adicionado quando dois projetos compartilham o mesmo nome de pasta. O canal de um projeto é alocado na primeira vez que ele planeja uma sessão, gravado em `lanes.toml` no diretório de configuração do offrig e mantido: o mesmo projeto recebe o mesmo canal após cada reinicialização. A alocação usa um arquivo de bloqueio e grava o registro atomicamente, para que dois side-cars iniciados simultaneamente nunca compartilhem uma tag, alias ou porta. Nenhum canal pode ser `11434` (a porta do Ollama local): o intervalo começa em `11500`, e uma configuração editada para indicar o contrário é recusada. Um plano registra seu canal, e seu lançamento, executor, watchdog e desligamento usam todos esse canal, e não a configuração global.

Um side-car sempre corresponde, lista ou interrompe apenas pods com o nome de seu próprio canal. O canal de outro projeto, os pods do canal padrão `offrig-<profile>` e qualquer outro pod na conta são deixados de lado: a verificação de lançamento para um pod ativo solicita apenas seu próprio canal, o desligamento recusa um pod cujo nome não seja o do canal do plano e a verificação de órfãos do túnel mata um `ssh` obsoleto apenas quando seu encaminhamento e seu alias são os do próprio canal. Planos criados antes da existência de canais não têm nenhum canal registrado e continuam sendo executados no canal padrão, portanto, um pod lançado sob o esquema antigo é desligado pelo mesmo plano que o iniciou.

**A própria porta do side-car.** `offrig-mcp` se comunica usando MCP via stdio. Um driver de shell que mantém uma conexão aberta durante toda a sessão (quando a própria conexão MCP da sessão está inativa) a coloca atrás de uma porta HTTP de loopback, e essa porta costumava ser um número para toda a máquina (`11439`): um segundo driver de projeto ou qualquer outro programa poderia usá-la e o primeiro side-car ficaria inativo sem emitir nenhum aviso. O padrão agora é por projeto, do canal do projeto, da mesma forma que a porta do túnel: o slot do canal `i` (porta do túnel `11500 + 2i`) recebe a porta do side-car `11700 + i`. O intervalo `11700` a `11763` está acima de todas as portas de túnel e executor que um canal pode ter (`11500` a `11627`), as `11435` e `11436` do canal padrão e o `11434` do Ollama local, para que uma porta de side-car nunca possa ser uma porta de túnel. Nada novo é armazenado: `lanes.toml` permanece inalterado e a porta é derivada do canal. `OFFRIG_SIDECAR_PORT` ainda a substitui; um valor que não seja uma porta, que seja inferior a 1024 ou que seja `11434`, `11435`, `11436` ou qualquer coisa no intervalo do túnel do canal é recusado.

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Com `--check`, uma porta ocupada é um erro que indica a porta e, quando um side-car "offrig"
responde ali, o projeto ao qual ele está servindo: `a porta do side-car 11700 está ocupada: um side-car offrig já está atendendo o projeto <path> ali. Pare-o primeiro ou defina OFFRIG_SIDECAR_PORT para uma
porta livre`. A verificação pergunta da mesma forma que o driver já responde (uma solicitação como um projeto
que ninguém está atendendo, o que o driver rejeita antes de tocar em qualquer ferramenta), então não muda nada
em um side-car em execução. `offrig_status` relata a `sidecar_port` da "lane".

**Um canal, um pod ativo.** Um canal tem um alias SSH e uma porta de túnel, portanto, ele atende a um pod por vez: um segundo pod no canal (`offrig-<tag>-job` ao lado de `offrig-<tag>-jam`) redefiniria o alias para ele mesmo e enviaria o `offrig_put`, `offrig_exec` e `offrig_get` do primeiro plano para a máquina errada. `offrig_launch`, portanto, recusa enquanto o canal tiver um plano aberto ou qualquer pod ativo que ele possua, com `lane <tag> has a live pod <name> (plan <id>); shut it down first`, before anything is committed or rented. The plain lane's `offrig up` e o aplicativo recusa da mesma forma para um pod de outro perfil (o pod do mesmo perfil ainda é reutilizado). Desligue o primeiro plano e, em seguida, inicie o próximo.

## Níveis

Os perfis residem em `%APPDATA%\offrig\config.toml` (gravados na primeira alteração). Padrões:

| Perfil | GPUs | Modelos | Custo típico |
|---|---|---|---|
| pequeno | 1 × RTX 2000 Ada / classe A4000 | `qwen3:4b` | aproximadamente US$ 0,25/hora |
| médio | 1 × RTX PRO 6000 (96 GB); A100 ou H100 80 GB se nenhum estiver livre | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | US$ 2,09/hora (A100 de reserva US$ 1,59) |
| de ponta | 4 × RTX PRO 6000 (384 GB), **SGLang** | Qwen3-Coder-480B AWQ 4 bits (252 GB), cerca de 130 GB restantes para o contexto | US$ 8,36/hora |
| frontier-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 GB): o caminho do mecanismo de "frontier", testado de forma econômica | aproximadamente US$ 1,7/hora |
| frontier-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 GB): os kernels de 4 bits MoE do "frontier", testados de forma econômica | aproximadamente US$ 1,7/hora |
| trabalho | 1 × RTX PRO 6000 (96 GB); A100 ou H100 80 GB se nenhum estiver livre | nenhum: um **pod de trabalho** executa seu trabalho, não um servidor de modelo | US$ 2,09/hora (A100 de reserva US$ 1,59) |
| congestionamento | 1 × A40 (48 GB) primeiro; A6000, A5000, 3090, L4 ou 4090 se nenhum estiver livre | nenhum: um **pod de trabalho** para as renderizações de canto das sessões de "ai-jam" (SoulX-Singer) | US$ 0,49/hora (A40) |

Um perfil com um `recipe` executa outro mecanismo diferente do Ollama: uma imagem fixada
(`lmsysorg/sglang:v0.5.20-cu130`), um modelo Hugging Face que ele baixa no início e
argumentos extras do servidor. O "offrig" define o paralelismo de tensor a partir da contagem de GPUs, o
comprimento do contexto a partir do perfil e mantém o mecanismo no loopback do pod; uma receita não pode
substituir esses valores. Para um repositório protegido, `hf_token_secret` nomeia um segredo RunPod, referenciado como
`{{ RUNPOD_SECRET_<name> }}` para que o token nunca entre nas especificações do pod. O lançamento relata
os pesos no disco enquanto o mecanismo os baixa e para imediatamente (com o log do mecanismo) se o mecanismo sair. Ele não declara que o mecanismo está pronto quando `/health` responde, e
`/v1/models` responder também não é prontidão. Após `/health` retornar 200, o "offrig"
envia uma conclusão de bate-papo (`max_tokens` 1, `temperature` 0) e aguarda até 30 segundos.
O passo é `ready` somente quando essa resposta tiver uma escolha. Se a conclusão falhar ou o tempo acabar, o "offrig" tenta mais uma vez após 15 segundos. Uma segunda falha é o passo
`engine_unresponsive`: o trabalho continua em execução e o erro e o log do mecanismo estão no
trabalho. A próxima ação é `o mecanismo não está respondendo às conclusões: desligue o pod ou aguarde se ele ainda estiver aquecendo`. O "offrig" não desliga o pod para isso, no
side-car, na CLI ou no aplicativo. Uma primeira solicitação lenta (captura de gráfico, JIT) não é um motivo
para descartar o download e o carregamento; o prazo do plano ainda limita o custo.
O relógio de espera começa em `grep -c` de `Application startup complete` na mesma leitura ssh
do rastreamento do log, em todo o log do mecanismo, não apenas nas últimas 40 linhas. Um mecanismo "chatty" pode empurrar a linha de inicialização para fora desse rastreamento. A contagem ainda inicia o relógio.
Se essa contagem for pelo menos um e `/health` permanecer inativa por 3 minutos, o passo é
`engine_hung`: as últimas 40 linhas, os segundos desde que a contagem foi vista pela primeira vez e a
próxima ação `shut the pod down: the engine finished startup but never became healthy`.
O "offrig" relata essa espera e deixa o pod ativo. Desligá-lo fica por sua conta,
assim como uma incompatibilidade na contagem de GPUs. Um aviso de contagem de GPU não é prefixado na próxima
ação, nem na ação não responsiva, para que a frase permaneça exata. Um processo de mecanismo que já saiu não é relatado. No side-car, o compensador exclui
esse pod. Na CLI e no aplicativo, o comando é interrompido com o erro e não o exclui.

Cada perfil lista os tipos de GPU por ordem de prioridade. v2 define um tipo e não recorre a outro,
por isso, o offrig tenta essa lista, por ordem, e nunca ultrapassa o preço do plano. Quando nenhum está
disponível, um perfil pode aguardar (`wait_for_gpu_minutes`; o frontier aguarda até 120 minutos): o offrig
verifica a cada minuto e cria o pod no momento em que as GPUs ficam disponíveis. Nada é alugado enquanto
ele aguarda, Ctrl+C ou o botão Cancelar do aplicativo interrompem o processo e, se o catálogo ativo estiver inativo, ele
simplesmente tenta criar novamente a cada minuto. Configurações grandes com várias GPUs surgem e desaparecem em minutos.
Os preços são os preços da lista de nuvem segura do catálogo v2 (`GET /catalog/gpus`), por GPU, multiplicados
pelo número de GPUs. Uma listagem de NENHUM não está disponível. A página de preços pública não é o preço disponível. `CUDA_VERSIONS` é apenas a alternativa quando a lista de uma GPU `cudaVersions` é omitida.

### Aguardando capacidade

Um plano restrito com `no_fallback` ou `max_price_hr` geralmente não tem capacidade, portanto, o lançamento tenta novamente silenciosamente em vez de falhar, não alugando nada enquanto o faz. Quanto tempo ele espera é, em ordem: o `wait_minutes` do plano (um argumento `offrig_plan`, armazenado com o plano; `0` falha imediatamente), caso contrário, o `wait_for_gpu_minutes` do perfil. O perfil `job` tem como padrão 20 minutos. A espera é reduzida para o tempo que o plano tem restante, menos uma reserva de cinco minutos, para que ele nunca seja executado após o prazo do plano e, como nada é alugado enquanto espera, não adiciona nada ao pior caso alocado. `offrig_launch` relata `capacity_wait_minutes`; enquanto espera, `offrig_job` mostra `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) e uma etapa que indica qual é esta verificação. Quando a espera termina, o lançamento falha com `no capacity` e nada foi alugado.

### Fixação do hardware de um plano

Um perfil lista os tipos de GPU por ordem de prioridade e RunPod seleciona o primeiro com capacidade, então,
sem limites, um plano pode ser executado em uma placa alternativa com menos memória, um driver mais antigo e um
preço diferente. Três limites restringem um plano ao hardware que seu trabalho pode usar. O planejamento ainda é
gratuito; os limites apenas restringem o que o plano pode alugar.

| Limite | Onde | Efeito |
|---|---|---|
| `min_cuda` | perfil (`config.toml`) | A versão CUDA (driver) mais antiga do host, da lista de RunPod (`13.2`, `13.1`, `13.0`, `12.9`, ... `11.8`). A criação do pod envia esse valor mínimo como `gpu.minCudaVersion`, então, um host mais recente do que a versão copiada mais recente ainda se qualifica. Quando o catálogo retorna `cudaVersions` para uma GPU, um plano mantém esse tipo apenas se alguma versão estiver disponível neste valor mínimo ou acima dele. Quando o catálogo omite a lista, a lista copiada é a alternativa. Para um perfil de trabalho, a versão mais recente deste e a versão própria da imagem `[profiles.job] min_cuda` se aplica. |
| `min_vram_gb` | perfil | A menor quantidade total de VRAM (todas as GPUs do perfil juntas) que um plano aceita. Ofertas abaixo disso são descartadas; um tipo RunPod que não lista memória também é descartado. |
| `max_price_hr` | argumento `offrig_plan` | O custo máximo que o pod pode ter, no total, em $/hora para todas as suas GPUs (o valor `offrig_offers` mostra). Ofertas acima desse valor são descartadas, assim como um tipo sem preço listado (não pode ser limitado a um valor máximo). |
| `no_fallback` | argumento `offrig_plan` | Apenas a primeira família de GPU do perfil é permitida. As duas edições RTX PRO 6000 Blackwell (Servidor e Workstation) são uma família; todas as outras placas, incluindo a A100 SXM e PCIe, são suas próprias famílias. |

Os campos do perfil são opcionais e, por padrão, não são definidos, portanto, um `config.toml` escrito por um
offrig anterior é carregado sem alterações. O perfil `job` define `min_cuda = "13.0"`.

`cuda_versions` é uma lista exata opcional, além de `min_cuda`, por exemplo,
`cuda_versions = ["13.0"]`. Quando está definido, a criação do pod envia essa lista como
`gpu.allowedCudaVersions`, na ordem em que foi escrita, e não envia também um valor mínimo.
`offrig_plan` relata a lista. Definir isso em conjunto com `min_cuda`, incluindo um valor mínimo próprio da imagem do trabalho, é recusado: os dois juntos são ambíguos. Cada versão deve ser uma que o offrig aceite. Essa lista, da mais recente para a mais antiga, é 13.2, 13.1, 13.0, depois 12.9 até
11.8. 13.2 e 13.1 estão à frente do v1 documentado de RunPod (lido em 2026-10-11, que
para em 13.0). Eles são listados porque um pod de gravação em 2026-10-11 foi colocado em um
host 13.2. Um valor mínimo ainda envia apenas `gpu.minCudaVersion`.

`offrig_plan` calcula o pior cenário com base no que resta:
`(max_hours + 780 s) x min(max_price_hr, the dearest listed price among the remaining GPUs)` (arredondado para cima, em centavos; 780 s é a margem de autointerrupção de 120 s no pod, mais 660 s de tempo de repetição). Sem
`max_price_hr`, esse é o preço listado mais caro no perfil, como antes. Um plano sem nada restante é recusado com o motivo de cada GPU descartada, e nada é escrito.

O plano armazena a lista de GPUs que restam e, ou o limite mínimo de CUDA ou a lista exata de CUDA, e `offrig_launch` aluga apenas o que o plano armazenou, nunca a partir de uma edição posterior
do perfil. `offrig_job` e o resultado do lançamento do side-car
relatam o tipo de GPU e a versão CUDA do host que foram realmente alugadas, em um objeto `rented`. A
API do pod não relata a versão CUDA do host, portanto, assim que o SSH estiver ativo, o lançamento é executado
`nvidia-smi` uma vez e lê os dados do cabeçalho (`CUDA Version: 12.8` ou
`CUDA UMD Version: 13.4` em drivers mais recentes); `rented.cuda_source` indica `nvidia-smi` ou
`pod API`. Se o CUDA do host for mais antigo do que o limite mínimo do plano, a GPU
não estiver na lista do plano ou o preço for superior ao do plano, `offrig_job` retorna uma entrada `warnings`
e inicia `next_action` com `WARNING`. Nada é encerrado automaticamente: interromper
o aluguel é responsabilidade de quem faz a chamada (`offrig_shutdown`). Se nem a API do pod nem `nvidia-smi`
fornecerem uma versão CUDA, `rented.notes` informa isso e o limite mínimo não é verificado.

### Disco do contêiner

Um pod tem dois discos: o disco do contêiner, local ao host, e o volume montado em
`/workspace`. Em alguns hosts, `/workspace` é um sistema de arquivos de rede lento: em um pod de trabalho, foram medidos
32 MB/s, em comparação com 354 MB/s no disco do contêiner, e não foi possível obter cerca de 130 GB de
modelos a tempo, enquanto o disco do contêiner tinha apenas 60 GB. O tamanho do disco do contêiner é o
`container_disk_gb` do perfil (de 30 a 60 GB nos perfis integrados; o perfil `job` tem
60) e é incluído na criação do pod como `disk`. `offrig_plan` usa
`container_disk_gb` (de 1 a 2000) para substituí-lo para um plano; o plano armazena, o lançamento
envia e a resposta do plano e `offrig_status` mostram o tamanho em vigor.

- O disco do contêiner não tem preço: o offrig cobra apenas pelo tempo de uso da GPU, portanto, o pior cenário do plano é o mesmo para qualquer tamanho. O RunPod cobra pelo disco; não foi verificado aqui se a taxa que ele informa para o pod (`offrig_job` mostra isso) inclui o disco do contêiner.
- O offrig não move seus downloads automaticamente. Os comandos de tarefa começam com `HF_HOME` no volume `/workspace` (`/workspace/hf`); para usar o disco do contêiner, defina o seu próprio (`HF_HOME=/root/hf python ...`) no comando.
- O disco do contêiner é excluído junto com o pod, como o volume sem um volume de rede: copie os resultados de volta com `offrig_get` antes de `offrig_shutdown`.
- O limite de 1 a 2000 é uma verificação interna do offrig para evitar erros de digitação; o limite real do RunPod não é verificado.

### Pods de trabalho

Um perfil com um `job` aluga uma GPU para tarefas que são executadas nele, como uma execução de treinamento, em vez de para servir um modelo. O pod executa uma imagem PyTorch fixa (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 para Blackwell) com sshd e nada mais:

- Ele não serve nenhum modelo, portanto, não há túnel e nada está conectado ao Zed. O sshd não permite nenhum tipo de encaminhamento (`AllowTcpForwarding=no`); a única maneira de acessar é via ssh para o pod.
- Um perfil de tarefa não lista nenhum modelo e também não pode ter uma receita; a verificação de configuração rejeita qualquer um dos dois.
- O `offrig up` e o aplicativo rejeitam um perfil de tarefa antes de alugar qualquer coisa. Ele é executado através do side-car: `offrig_plan profile=job`, `offrig_launch`, depois `offrig_put`, `offrig_exec` e `offrig_get`. O lançamento está pronto quando o sshd responde.
- Um comando é executado de forma independente no pod (`setsid nohup`) em `/workspace/job`, portanto, ele sobrevive ao side-car e à sessão ssh. É enviado como base64, portanto, nada nele é lido pelo shell ssh. Seu log e status de saída permanecem em `/workspace/offrig/jobs/`. Os downloads do Hugging Face vão para `/workspace/hf` no volume do pod.
- O `offrig_exec action=run` é para verificações rápidas (`ls`, `nvidia-smi`), não para tarefas: ele executa o comando até a conclusão sob `timeout` (padrão de 30 s, no máximo 120 s) e retorna `stdout`, `stderr`, `exit_code` e `timed_out`. A saída é cortada para os últimos 64 KB de cada fluxo (`truncated`) e é uma saída de pod não confiável. Um comando que precisa de mais tempo é um `start`.
- O final do log de uma tarefa tem barras de progresso recolhidas: as redesenhos no estilo tqdm, unidos por retornos de carro, mostram apenas seu último quadro. O `offrig_exec action=status save_log=<local path>` também copia todo o log da tarefa, conforme escrito, para um arquivo local (as pastas pai são criadas), para que o final possa permanecer curto.
- A imagem é uma compilação CUDA 12.8, portanto, um perfil de tarefa especifica a versão CUDA mais antiga do host em que ele é executado (`min_cuda = "12.8"`) e o pod é criado com esse valor mínimo como `gpu.minCudaVersion`. Sem isso, um host com um driver mais antigo inicia o pod e o torch não encontra nenhuma GPU, depois que o aluguel é iniciado. O trabalho em si pode precisar de mais do que a imagem: o perfil `job` também define `min_cuda = "13.0"` no perfil (veja acima), porque as tarefas que ele executa instalam um vLLM atual, cujo PyTorch é uma compilação CUDA 13.
- Orçamento, plano, watchdog e desligamento funcionam como em todos os outros perfis. Copie os resultados de volta antes de `offrig_shutdown`: o disco do pod vai junto com ele.
- O `jam` é o perfil de tarefa que o ai-jam-sessions usa para renderizar suas músicas: o SoulX-Singer precisa de muito menos do que uma placa de treinamento, portanto, ele aluga uma placa barata de 24 a 48 GB. A configuração e a sessão estão nesse repositório (`docs/vocal-offrig.md`); o offrig não sabe nada sobre música.

### Pesos em fase de preparação em um volume de rede

Um perfil de receita baixa seus pesos em cada lançamento: para o modelo de ponta, isso levava cerca de 20/22 minutos para estar pronto (252 GB, US$ 8,36/hora). A preparação os coloca em um volume de rede RunPod uma vez:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- O volume é cobrado mensalmente, independentemente de um pod estar em execução ou não (300 GB para o modelo de ponta custa cerca de US$ 21/mês a US$ 0,07/GB), portanto, apenas um humano faz a preparação; nenhuma ferramenta de agente pode.
- Um volume reside em um único data center, portanto, os pods do perfil são iniciados apenas lá, e as ofertas e os planos são precificados lá. Escolha um com armazenamento em rede e as GPUs do perfil; o console do `offrig gpus` e do RunPod mostra onde eles estão.
- O download é executado no pod de GPU mais barato disponível naquele data center. O pod é encerrado em caso de sucesso, falha ou tempo limite.
- O volume é registrado no perfil antes que o download comece, portanto, uma preparação com falha nunca é esquecida; execute novamente para retomar ou `--remove`.
- Um lançamento preparado executa o Hugging Face offline, apenas quando a preparação é concluída (um marcador no volume). Um volume parcialmente preparado baixa o restante em vez de falhar.

## Segurança financeira

- Antes de um lançamento, o offrig mostra a correspondência gratuita mais barata e seu tempo de execução com o pod em execução. Se o tempo de execução for inferior a uma hora, ele se recusa, a menos que você o substitua, porque, quando chega a zero, o RunPod interrompe todos os pods na conta, incluindo aqueles que o offrig não gerencia.
- O desligamento automático encerra o pod após 30 minutos com todas as GPUs abaixo de 5% (configurável ou desativado).
- Fechar o aplicativo com um pod em execução pergunta se você deseja encerrá-lo ou mantê-lo em execução.
- O offrig só toca nos pods que ele nomeou: `offrig-<profile>` para a CLI e o aplicativo, `offrig-<tag>-<profile>` para a faixa side-car de um projeto (veja Faixas). Um side-car nunca toca nos pods de outra faixa, na faixa simples ou em qualquer outro pod; esses são listados, nunca alterados.
- Para sessões de agente, o limite é aplicado antes de qualquer gasto: o pior cenário de um plano (preço atual × (horas máximas + 780 s de tempo de espera para desligamento automático), arredondado para centavos) é comparado com o orçamento definido pelo usuário e é rejeitado se exceder o orçamento, e um lançamento recebe apenas um ID de plano, portanto, um agente não pode definir seu próprio preço.
- Cada lançamento side-car tem um watchdog que encerra o pod no prazo do plano, e o executor desliga o pod assim que sua fila estiver vazia.

## O que ele altera na sua máquina

| O quê | Onde | Desfazer |
|---|---|---|
| Provedor Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; o primeiro original é mantido como `settings.json.offrig.bak` |
| Modelo padrão do Zed (apenas se você solicitar) | mesmo arquivo | `offrig zed-remove` restaura o padrão anterior |
| `OFFRIG_API_KEY` (espaço reservado; o Zed precisa de uma chave) | ambiente do usuário | `setx OFFRIG_API_KEY ""` ou remova-o nas Propriedades do Sistema |
| Alias SSH `offrig` | `~/.ssh/config`, entre os marcadores `# >>> offrig:offrig >>>` | exclua o bloco marcado |
| Alias SSH `offrig-<tag>`, um por projeto que foi iniciado a partir de um side-car | `~/.ssh/config`, entre os marcadores `# >>> offrig:offrig-<tag> >>>` | exclua o bloco marcado |
| Caminhos do projeto | `%APPDATA%\offrig\lanes.toml` (caminho do projeto, tag, alias, porta do túnel) | exclua a entrada do projeto enquanto nenhum pod estiver em execução em seu caminho, ou o arquivo inteiro |
| Chaves do host do pod | `~/.ssh/known_hosts_offrig` | exclua o arquivo |
| Configurações | `%APPDATA%\offrig\config.toml` | exclua o arquivo |
| Pesos em fase de preparação (apenas com `offrig stage --yes`) | um volume de rede RunPod `offrig-<profile>`; cobrança mensal | `offrig stage <profile> --remove --yes` |

Os comentários e o layout nas configurações do Zed são preservados: as edições passam por uma árvore de sintaxe JSONC.

## Modelo de ameaças

- **Chave da API RunPod.** Lida de `RUNPOD_API_KEY`; nunca gravada em disco ou logs. O provedor Zed não é intencionalmente nomeado `runpod`, porque o Zed então leria `RUNPOD_API_KEY` e o enviaria para o servidor do modelo.
- **Servidor do modelo.** Acessível apenas via SSH com sua chave. O login por senha está desativado no pod, e o sshd permite apenas o encaminhamento local.
- **Chaves do host.** Fixadas por ponto de extremidade em um arquivo separado de "known-hosts". O offrig esquece uma chave apenas quando o ponto de extremidade do pod muda, porque RunPod reutiliza pares de ip:porta entre os pods.
- **Injeção de shell.** Os nomes dos modelos são validados em relação à sintaxe de nomes do Ollama antes de chegarem a um shell remoto.
- **Túneis órfãos.** Se o offrig falhar, seu `ssh` pode continuar mantendo a porta. Na próxima inicialização, o offrig o encerra, mas apenas se o listener estiver `ssh.exe` carregando as especificações exatas de encaminhamento do offrig. Qualquer outra coisa na porta é recusada, nunca encerrada.
- **Sem telemetria.** O offrig se comunica apenas com a API do RunPod, seu pod e seu Ollama local (para comparar as listas de modelos).

## Testes

`cargo test --workspace` executa mais de 250 testes, cobrindo pelo menos 90% das linhas (o CI falha abaixo desse valor):

- **A biblioteca principal:** análise de RunPod, especificações de pod para ambos os mecanismos, configuração SSH, edições JSONC do Zed, regras de proteção, lógica de custo e inatividade, o armazenamento e suas migrações, funções, montagem de contexto, verificações determinísticas, as decisões do executor, o watchdog e o estágio, incluindo um RunPod simulado que prova que um estágio com falha encerra seu pod.
- **O aplicativo:** gerenciamento de estado mais testes de interface do usuário de clique em egui.
- **A CLI:** códigos de saída, níveis de log e que a chave da API nunca apareça na saída.
- **O side-car:** teste completo via stdio contra um RunPod simulado, o processo real do watchdog e o processo real do executor contra um modelo de pod simulado; cada erro de ferramenta carrega um código.

`scripts/verify.sh` (ou `scripts/verify.ps1`) executa a verificação de formato, clippy, os testes e
uma execução de teste de cada binário em um único comando. O CI também executa `cargo deny`, uma verificação OSV de
`Cargo.lock`, cobertura para o Codecov e `atlas check`.

### Registro de teste ao vivo (2026-10-02, nível médio, A100 80 GB, cerca de US$ 0,45)

- Pod ativo em cerca de 80 segundos; sshd, túnel e o Ollama 0.35.0 do pod respondendo.
- 97 GB de modelos baixados no pod a cerca de 150–250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` e `gpt-oss:120b` responderam a um chat transmitido com um
chamado de ferramenta correto através do túnel. Eles usaram 36 GB e 64 GB da VRAM do pod; o
Ollama local não carregou nada e não estava na GPU local.
- Todas as sete verificações de segurança foram aprovadas, a partir da CLI e do aplicativo.
- Uma CLI interrompida abruptamente deixou seu `ssh` mantendo a porta; a próxima execução a recuperou.
- O túnel, as verificações, o teste de modelo e o desligamento do aplicativo foram executados por meio de seus botões.

Bugs encontrados durante a execução ao vivo, agora corrigidos e cobertos: uma lista `&&` em segundo plano manteve o stdout do ssh aberto e interrompeu o início do download; a lista de pods não continha tipos de GPU sem
`includeMachine=true`; a verificação de lançamento contava o preço de um pod em execução duas vezes.

### Ensaio do side-car (2026-10-03, nível pequeno, RTX 2000 Ada, $0,08 reservado)

O `offrig-mcp` instalado é executado por meio do stdio, da mesma forma que um agente o chama:

- `offrig_plan` calculou 0,5 h a US$ 0,15 no pior caso; `offrig_launch` confirmou, iniciou o watchdog e uma segunda chamada retornou o mesmo trabalho. Um pod foi alugado, a US$ 0,24/h.
- SSH foi iniciado 100 segundos após o aluguel, `qwen3:4b` baixado, pronto em 150 segundos.
- `offrig_ask` executou uma transferência de designer de jogos em 46 segundos; a resposta atendeu à sua verificação de aceitação e manteve a restrição de cinco itens da memória.
- O pod acessou a internet (Wikipedia, API GitHub). Dados os dados obtidos pelo pod, o modelo respondeu corretamente às perguntas atuais; quando perguntado sem contexto, disse que não tinha acesso ao vivo.
- `offrig_shutdown` de um processo side-car recém-criado encerrou o pod e fechou as contas; o watchdog viu o plano ser fechado e saiu. A GPU local permaneceu ociosa durante todo o processo.

Encontrado e corrigido: o pod atendia a um pedido por vez (`OLLAMA_NUM_PARALLEL=1`); quatro
slots receberam 8 solicitações paralelas de 40 a 102 tok/s na mesma GPU, portanto, agora cada perfil tem
`parallel = 4`. `complete` foi recusado sem motivo (agora, por padrão, é
"verificação de aceitação aprovada"; as falhas ainda precisam de um motivo). O status sugeriu registrar um breve
período enquanto uma sessão estava ativa. O texto que vaza para uma resposta é removido, e uma resposta
vazia causada por um pensamento indica que `max_tokens` deve ser aumentado.

### Ensaio do executor (2026-10-03, nível pequeno, RTX 2000 Ada, $0,04 reservado)

Cinco transferências, uma dependendo da outra, funcionaram por meio de `offrig_run` sem que ninguém as controlasse:

- Quatro transferências em andamento ao mesmo tempo em quatro slots (12,9 GB de 16 GB de VRAM); a dependente
começou no momento em que sua dependência foi concluída e foi construída com base em seu resultado.
- As três transferências cujas verificações cobriram a aceitação foram concluídas por conta própria; as histórias de fundo rivais (verificações parciais) e a história (sem verificações) foram enviadas para revisão.
- A revisão enviou a história de volta ("o rio recebeu o nome do projeto"); o executor ativo
a adotou e a revisou com base no feedback ("Rio Veyl").
- A fila foi esvaziada em 6,5 minutos (6 turnos, 20.861 tokens); o executor desligou o pod
sozinho.

Aprendido: as verificações determinísticas verificam a estrutura, não a qualidade do design. O modelo de 4B passou por "três verbos" com verbos fracos, então `accept_on_checks` é para trabalho estrutural e o trabalho de design vai para revisão. qwen3:4b gastou cerca de 4.000 tokens pensando por turno, mesmo em três linhas de conhecimento. Uma fila mantém cada slot ocupado apenas quando contém transferências independentes suficientes; uma cadeia de dependência é executada uma de cada vez.

### Execuções de Frontier e SGLang (2026-10-03, US$ 4,27 reservado)

| Executar | Pod | Pronto após | Fila | Reservado |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 min | 4 transferências em 15 s | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 min | 4 transferências | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 min** (252 GB a 278 MB/s, depois carregamento) | **31 transferências em 20 s** | **$3.59** |
| 30B, varredura em lote | 1 × RTX PRO 6000 | 10 min (colocação lenta de pods) | apenas varredura | $0.43 |

- SGLang v0.5.20 (cu130) é executado em Blackwell: atenção flashinfer, `awq_marlin` para os
pesos de 4 bits MoE, paralelismo de tensores via PCIe em quatro placas; `/dev/shm` era 352 GB.
- O trabalho do frontier foi claramente melhor do que o dos modelos menores: diálogos no ambiente e um
módulo Rust que foi compilado e passou em seus três testes (verificado localmente). A versão da mesma tarefa da execução de 30B FP8 não foi compilada.
- Um modelo carregado atende a todo um enxame; não são necessárias cópias. Varredura de concorrência com
respostas de 384 tokens, total de tokens por segundo:

| Agentes | 480B em 4 GPUs | 30B AWQ em 1 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

A velocidade por agente diminui à medida que os agentes são adicionados (480B: 88 → 24 tok/s em 64), mas o
desempenho total continua a aumentar; os ganhos do 480B se estabilizam após 256. O cache KV do frontier
armazena 398.526 tokens, portanto, com contextos de transferência reais de 2 a 8 mil tokens, o nível do frontier
agora executa 64 em paralelo e os níveis de uma única placa SGLang executam 32.

Encontrado e corrigido ao longo do processo: as revisões preenchiam a saída para passar em uma verificação de cabeçalho (agora uma verificação `no_repeats` integrada e as revisões reestruturam no local); a saída necessária de uma função vazava para os resultados (a transferência agora define o formato); o feedback do cabeçalho nomeia o formato Markdown; uma revisão inalterada para em vez de repetir.

Ainda não verificado em tempo real: um chat
enviado diretamente do painel do agente do Zed (o formato de solicitação que o Zed usa é testado diretamente) e a verificação de pronto 1.1.0, relatório de travamento e pin CUDA exato em um pod alugado (veja
[Status](#status)).

## Conformidade com os padrões

Pontuação em relação aos padrões de fluxo de trabalho do estúdio (0 ausentes, 1 parciais, 2 presentes, 3 exemplares).

- **PIN_PER_STEP: 2.** As imagens do pod são fixadas em tags de versão (`ollama/ollama:0.35.0`,
`lmsysorg/sglang:v0.5.20-cu130`; uma receita recusa `latest`), o compilador na versão 1.98.1,
as dependências por `Cargo.lock` e o mecanismo Atlas para a versão 1.24.0 da frota. Cada etapa de transferência
registra seu modelo, hash de papel e hash de prompt. Os modelos são fixados por tag ou ID de repositório,
não por hash.
- **ANDON_AUTHORITY: 3.** Cada etapa interrompe a execução em caso de defeito: um plano cujos pesos
excedem o disco é rejeitado antes de qualquer gasto; uma falha no download interrompe o lançamento; uma edição do Zed
que não pode ser lida não é gravada; um arquivo de configurações com problemas é relatado, nunca
reescrito; o CI bloqueia em fmt, clippy, testes, licenças e avisos.
- **NAMED_COMPENSATORS: 2.** Cada ação irreversível tem um "desfazer", listado abaixo.
- **DECOMPOSE_BY_SECRETS: 2.** Um módulo por coisa que muda por seus próprios motivos:
a API do RunPod (`runpod`), o conteúdo do pod (`spec`), o transporte (`tunnel`,
`remote`), cada arquivo local que o offrig edita (`sshconfig`, `zed`) e as regras (`guard`,
`cost`). As interfaces mantêm nenhuma lógica além da apresentação.
- **UNCERTAINTY_GATED_HUMANS: 2.** O offrig pergunta apenas onde o resultado é caro ou
com perdas: lançamento com menos de uma hora de tempo disponível, término de um pod (com o que é perdido especificado) e
saída com um pod ainda em execução. Duas decisões pertencem apenas a um humano, e nenhuma
ferramenta de agente pode tomá-las: o limite de orçamento e o preparo de um volume, que são cobrados mensalmente.
A saída da transferência que o código não pode verificar aguarda em revisão em vez de ser concluída.
- **EXTERNAL_VERIFIER: n/a.** Nenhuma reivindicação especializada.

**Compensadores**

| Ação | Desfazer | Estado após desfazer | Proprietário |
|---|---|---|---|
| Criar um pod (inicia a cobrança) | `offrig down <profile> --yes`, o desligamento do aplicativo ou desligamento automático | pod encerrado, cobrança interrompida | o operador executando o offrig |
| Encerrar um pod | nenhum para o disco; relançar o perfil e os modelos são baixados novamente (um volume de rede os mantém) | novo pod, mesmo perfil | o operador |
| Escrever o provedor Zed ou o modelo padrão | `offrig zed-remove` ou restaurar `settings.json.offrig.bak` | Zed como antes, offrig | o operador |
| Definir `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` ou excluí-lo nas Propriedades do Sistema | variável removida | o operador |
| Escrever o alias SSH | excluir o bloco marcado em `~/.ssh/config` | configuração como antes | o operador |
| Alocar uma faixa de projeto (o primeiro `offrig_plan` do projeto) | excluir a entrada do projeto de `lanes.toml` quando nenhum pod estiver em execução em sua faixa; um novo plano aloca novamente | faixa livre para reutilização; o bloco de alias é separado (linha acima) | o operador |
| Escrever um bloco de alias SSH de uma faixa (um lançamento de side-car) | `offrig_shutdown` o remove quando nomeia o pod do plano (também após um lançamento com falha); caso contrário, excluir o bloco `# >>> offrig:offrig-<tag> >>>` em `~/.ssh/config` | configuração como antes; os blocos de outras faixas não são alterados | o operador |
| Baixar um modelo no pod | `ollama rm <model>` no pod ou encerrar o pod | modelo removido | o operador |
| Encerrar um túnel offrig órfão | nenhum necessário; apenas um `ssh` com o alias e o encaminhamento exatos desta faixa é encerrado, nunca de outra faixa | porta livre | offrig |
| Lançamento de side-car (`offrig_launch`) | `offrig_shutdown`; automático se a configuração falhar; o watchdog no prazo final; o executor quando sua fila esvaziar | pod encerrado, plano fechado com gastos medidos | o agente que faz a chamada, com o watchdog como proteção |
| Preparar um volume (`offrig stage --yes`, cobra mensalmente) | `offrig stage <profile> --remove --yes` | volume excluído, perfil voltando a baixar | o usuário que o preparou |
| Iniciar um trabalho em um pod de trabalho (`offrig_exec action=start`) | `offrig_exec action=stop` ou `offrig_shutdown` | trabalho encerrado com tudo o que iniciou; seu log permanece até que o pod seja removido | o agente que faz a chamada |
| Executar um comando curto em um pod de trabalho (`offrig_exec action=run`) | apenas o que o próprio comando faz; encerrado por `timeout` no máximo em 120 s ou por `offrig_shutdown` | o pod como o comando o deixou | o agente que faz a chamada |
| Copiar arquivos para ou de um pod de trabalho (`offrig_put`, `offrig_get`) | excluir a cópia (no pod, `offrig_exec`; aqui, o arquivo) | como antes da cópia | o agente que faz a chamada |

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

## Licença

MIT. Consulte [LICENSE](LICENSE).

---

Criado por <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>
