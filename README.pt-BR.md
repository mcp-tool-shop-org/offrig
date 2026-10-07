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

Execute modelos grandes em GPUs alugadas da RunPod, com a garantia de que eles nunca serão executados em sua própria GPU. Um aplicativo para desktop, uma interface de linha de comando (CLI) e um componente adicional (side-car) para agentes, tudo em uma única biblioteca Rust.

O componente adicional permite que um agente planeje uma sessão paga dentro de um orçamento definido pelo usuário, alugue as GPUs e envie uma fila de tarefas para um executor independente. O executor mantém todos os slots de modelo ocupados, revisa apenas as verificações que falharam e desliga o pod quando a fila estiver vazia. Um sistema de monitoramento encerra o pod no prazo definido no plano, mesmo que tudo o mais esteja inativo.

## Status

Testado com sucesso em 2026-10-02 e 2026-10-03, com um custo total de cerca de US$ 5:

- **Frontier:** 4 × RTX PRO 6000 (384 GB) executando o Qwen3-Coder-480B (4-bit AWQ) no SGLang, pronto em 22 minutos, seguido por 31 transferências de tarefas em 20 segundos, com um custo de US$ 3,59.
- **Dados do Swarm:** um modelo carregado atende a centenas de agentes simultaneamente. O modelo de 480B atingiu 4.059 tokens/s com 512 agentes; um modelo de 30B em uma única placa atingiu 10.147 tokens/s com 256.
- **Executor:** uma fila com dependências e reenvios de revisão, funcionando sem intervenção manual; o pod é desligado quando a fila está vazia.
- **Garantia:** a GPU local permanece inativa durante todas as execuções.

Em uso diário desde 2026-10-07 por dois projetos simultaneamente, cada um em seu próprio ambiente: execuções de treinamento para aspire-si em `job` pods e renderizações de músicas para ai-jam-sessions em `jam` pods.

Construído e testado, aguardando uma decisão: preparação dos pesos do Frontier em um volume de rede, com um custo de cerca de US$ 21 por mês (veja [Preparação](#preparação-dos-pesos-em-um-volume-de-rede)).

Próximo: a primeira fila real do Frontier, planejada integralmente antes do lançamento; transferências de código compiladas e testadas no pod.

## O que ele faz

De uma única janela (ou um único comando), o offrig:

1. exibe seu saldo da RunPod, os preços atuais das GPUs e por quanto tempo o saldo durará;
2. inicia um pod para um determinado nível, desde 1 placa pequena no Ollama até 4 × RTX PRO 6000 no SGLang, e carrega seus modelos no pod;
3. abre um túnel SSH para o pod;
4. adiciona os modelos do pod ao Zed como seu próprio provedor;
5. executa sete verificações para garantir que os modelos não sejam executados nesta máquina;
6. desliga o pod ou o encerra após um período com todas as GPUs inativas.

Através do componente adicional, um agente também planeja sessões dentro de um orçamento, mantém a memória do projeto entre compactações e reinicializações e executa filas de transferência sem intervenção manual (veja [O componente adicional](#o-componente-adicional-para-agentes)).

## A garantia e como ela é mantida

- **O servidor de modelo é inacessível, exceto através do túnel.** O pod executa um motor fixo, Ollama (`ollama/ollama:0.35.0`) ou SGLang (`lmsysorg/sglang:v0.5.20-cu130`), vinculado ao próprio loopback do pod, e o pod expõe apenas `22/tcp`. Não há nenhum ponto de extremidade HTTP público para encontrar ou abusar. Um script não pode mover o motor para fora do loopback.
- **O Zed se comunica com o túnel, em sua própria porta.** O túnel escuta na porta `127.0.0.1:11435`. Seu Ollama local está na porta `11434`. O offrig se recusa a colocar o túnel na porta `11434`, para que um túnel inativo não possa se conectar ao servidor local: a solicitação falha.
- **O Zed nunca alterna provedores.** Os modelos do pod são um provedor `offrig` separado no Zed. Se o pod estiver inativo, a seleção de um deles resultará em um erro; o Zed não tentará outro provedor.
- **Os pesos nunca existem localmente.** Os modelos são carregados no pod, pelo próprio pod (ou baixados de lá do Hugging Face, ou lidos de um volume de rede preparado).

As verificações de segurança verificam isso a cada vez, com base em fatos que o offrig pode observar:

| Verificação | Falha quando |
|---|---|
| O túnel evita a porta local do Ollama | a porta do túnel é 11434 |
| O Zed envia os modelos do pod através do túnel | a URL do provedor do Zed é diferente do túnel |
| O Ollama do pod não está exposto à internet | o pod mapeia a porta 11434 publicamente |
| O túnel termina no pod | a lista de modelos através do túnel é diferente da lista lida no pod via SSH |
| Os modelos do pod não estão nesta máquina | um modelo do pod também existe no Ollama local |
| Nenhum modelo do pod compartilha um nome com um modelo local do Zed | um nome no provedor offrig também está na lista do Ollama local do Zed |
| Todos os modelos que o Zed oferece estão no pod | O Zed oferece um modelo que o pod não tem |

## Instalação

Requer Windows com OpenSSH (integrado), Zed se você quiser os modelos em um editor e uma conta RunPod.

1. Baixe `offrig-<version>-windows-x64.zip` de [Releases](https://github.com/mcp-tool-shop-org/offrig/releases), verifique-o em relação ao `SHA256SUMS` da versão e descompacte-o em seu `PATH`. Ele contém `offrig.exe` (a CLI), `offrig-app.exe` (o aplicativo) e `offrig-mcp.exe` (o componente adicional). Para construir a partir do código-fonte: `cargo build --release`, com Rust 1.98.1 (fixado em `rust-toolchain.toml`).
2. Coloque sua chave de API da RunPod na variável de ambiente do usuário `RUNPOD_API_KEY`.
3. Adicione sua chave pública SSH nas configurações da conta da RunPod. O offrig usa `~/.ssh/runpod_rustline` se presente, caso contrário, `~/.ssh/id_ed25519`.
4. Para agentes, registre o componente adicional no Claude Code no escopo do usuário: `claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Ele abre o armazenamento de um projeto apenas na primeira vez, portanto, é inofensivo em projetos que nunca o usam.

O [manual](https://mcp-tool-shop-org.github.io/offrig/handbook/) explica o primeiro pod, o componente adicional, a configuração, os ambientes e os pods de trabalho.

## Uso

**Aplicativo:** inicie `offrig-app`, escolha um perfil e pressione **Iniciar pod**. Quando estiver pronto, os modelos aparecerão no painel de agentes do Zed como "RunPod · …". Reinicie o Zed uma vez após o primeiro lançamento para que ele veja `OFFRIG_API_KEY`.

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
offrig stage frontier --dc EUR-IS-1 --yes   stage weights on a network volume (bills monthly)
```

### Saída, códigos de saída e erros

- **Níveis de registro:** `-q` imprime apenas erros e os resultados de um comando; `-v` adiciona cada chamada RunPod e seu tempo de execução; `--debug` adiciona os corpos de resposta com falha e as cadeias de erros completas. A chave da API é omitida em todos os níveis.
- **Códigos de saída:** `0` sucesso, `1` algo para corrigir do seu lado (argumentos, configuração, uma restrição ou recusa de orçamento, uma chave ausente), `2` uma falha em tempo de execução (RunPod, rede, ssh, tempo limite, sem capacidade).
- **Erros do side-car** são resultados, nunca erros de protocolo: `ok:false` com um `code` estável, o texto `error`, um `next_action` e `retryable`. Os códigos estão listados no [referencial do manual](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## O side-car (para agentes)

`offrig-mcp` é um servidor MCP que um agente, como o Claude Code, chama como um instrumento. Ele mantém um banco de dados por projeto em `<project>/.offrig/offrig.db` que sobrevive a todos os pods, para que uma sessão sobreviva à compactação ou a uma reinicialização sem precisar explicar tudo novamente.

| Ferramenta | O que ele faz |
|---|---|
| `offrig_status` | O projeto, o orçamento, o saldo e o tempo de execução do RunPod, os pods do offrig, cada plano aberto com seu `plan_id`, faixa e nome do pod, a fila de transferência com trabalhos antigos sinalizados. |
| `offrig_offers` | Ofertas de GPU ao vivo para uma contagem de GPUs |
| `offrig_plan` | Define o preço de uma sessão no pior cenário (preço ao vivo x horas máximas); recusado se exceder o orçamento restante. A resposta indica o `ssh_alias` da faixa e o `pod_name` que o lançamento criará, e o `container_disk_gb` que solicitará (o `container_disk_gb` opcional substitui o perfil; consulte "Disco do contêiner"). O `max_price_hr` e o `no_fallback` opcionais restringem quais GPUs ele pode alugar (consulte "Fixação do hardware de um plano"); o `wait_minutes` opcional define por quanto tempo o lançamento tenta novamente quando não há capacidade (consulte "Aguardando capacidade"). |
| `offrig_memory_search` | Pesquisa a memória ativa do projeto, cada resultado com fonte e data |
| `offrig_memory_record` | Adiciona um resumo, restrição, decisão, fato ou ponto de verificação; as alterações são substituições com um motivo |
| `offrig_handoffs` | Coloca em fila as transferências com base em funções (cada uma precisa de uma verificação de aceitação; verificações determinísticas opcionais), lista-as, visualiza os blocos de função, mostra a melhor saída de uma transferência (também gravada em `.offrig/out/`), registra os resultados (concluído, inválido, violação, falha, nova tentativa com feedback) |
| `offrig_launch` | **Gasta.** Aceita apenas um `plan_id`: confirma o pior cenário, aguarda as GPUs alugando nada, inicia o pod, abre o túnel, importa os modelos, inicia o watchdog. Idempotente por plano. Recusado enquanto a faixa já tem um plano ou pod ativo: `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | Progresso do lançamento (enquanto o pod é iniciado, o passo é derivado do estado do pod no momento da chamada; cada nova tentativa de capacidade é contada em `progress.capacity_wait`), o tipo de GPU e a versão CUDA do host realmente alugados (medidos com `nvidia-smi` no pod, com uma entrada alta `warnings` quando o host é mais antigo do que o limite CUDA do plano), o watchdog ativo, minutos restantes, gasto até o momento |
| `offrig_ask` | Uma rodada de uma transferência no modelo do pod, o contexto é construído a partir do armazenamento do projeto; a resposta é retornada como uma saída não confiável |
| `offrig_run` | Inicia um executor separado que mantém cada slot de modelo ocupado: elabora cada transferência pronta, revisa no máximo duas vezes em relação a verificações com falha, alimenta os resultados para as transferências dependentes e, em seguida, desliga o pod quando a fila estiver vazia (a menos que `keep_pod`). O trabalho que o código não pode verificar aguarda em revisão |
| `offrig_put` | Copia um arquivo ou diretório local para um pod de trabalho (scp); os caminhos relativos do pod estão em `/workspace/job`. O `plan_id` opcional (veja abaixo) |
| `offrig_exec` | Executa um comando bash em um pod de trabalho, de forma independente, para que ele sobreviva ao side-car (`start`), relata se está em execução ou se foi encerrado, com seu código de saída e o final do log (`status`; `save_log` também copia todo o log para um arquivo local), o encerra (`stop`) ou executa um comando curto agora e retorna sua saída padrão, erro padrão e código de saída (`run`, `timeout_secs` padrão de 30, no máximo 120). O `plan_id` opcional (veja abaixo) |
| `offrig_get` | Copia um arquivo ou diretório de volta de um pod de trabalho, criando pastas pai locais ausentes; faça isso antes do desligamento, que exclui o disco do pod. O `plan_id` opcional (veja abaixo) |
| `offrig_shutdown` | **Destrói o pod.** O encerra e fecha os livros do plano com o gasto medido; recusado enquanto houver transferências em andamento, a menos que seja fornecido um motivo. Remove o bloco `~/.ssh/config` da faixa quando ele nomeia esse pod (`ssh_block_removed`) |

**Em qual plano de trabalho uma ferramenta de trabalho atua.** `offrig_put`, `offrig_exec` e `offrig_get` aceitam um `plan_id` opcional. Com exatamente um plano de trabalho aberto e nenhum `plan_id`, eles o usam, como antes. Com mais de um plano de trabalho aberto e nenhum `plan_id`, eles se recusam e listam os planos abertos (ID, perfil, nome do pod): eles nunca adivinham. Com um `plan_id`, eles atuam apenas no pod desse plano e apenas depois de verificar se o nome do pod é o que esse plano possui (um pod de outra faixa é recusado, não apenas de outra faixa). Cada resposta da ferramenta de trabalho e `offrig_job` indica o `project` e o `plan_id` em que atuou (as respostas da ferramenta de trabalho também indicam o `lane`); `offrig_status` indica o `project` e lista todos os planos abertos com seu `plan_id`, faixa e nome do pod.

As funções vêm do Role OS (dossiês e cartões de pacote inicial) mais quatro funções de jogo enviadas aqui nos formatos do Role OS: designer de jogos, designer de sistemas, designer narrativo, guardião da história. O limite do orçamento é definido apenas por um humano:

```text
offrig budget 15          set this project's cap (run in the project directory)
offrig budget             show cap, committed, spent, remaining
```

Cada lançamento inicia um **watchdog**: um processo separado que encerra o pod no prazo do plano (tempo confirmado + horas máximas), mesmo que o agente, a sessão ou o side-car tenham desaparecido. Ele nunca atua em uma pesquisa com falha, é encerrado exatamente uma vez, fecha os livros e registra em `.offrig/watchdog-<plan>.log`. Se a preparação de um pod alugado falhar, o lançamento o encerra em vez de deixá-lo cobrando.

O design e suas evidências estão em [docs/sidecar-design.md](docs/sidecar-design.md).

### Faixas: um side-car por projeto, sem colisões

Dois projetos podem executar side-cars simultaneamente em uma conta RunPod. Cada projeto recebe sua própria **faixa**: um alias SSH, uma porta de túnel e uma tag de nome de pod que nenhum outro projeto compartilha.

| | Faixa simples (a CLI, o aplicativo, Zed) | A faixa de um projeto |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Porta de túnel | `11435` (executor `11436`) | primeiro livre de `11500`, `11502`, ... (executor: a porta acima) |
| Porta do side-car (driver de shell) | nenhum | `11700` + o slot da fila: `11700`, `11701`, ... |
| Nome do Pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Bloco SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` vem do nome da pasta do projeto (`aspire-si`, `ai-jam-sessions`), com um pequeno
hash adicionado quando dois projetos compartilham o mesmo nome de pasta. A fila de um projeto é alocada na primeira
vez que ele planeja uma sessão, escrita em `lanes.toml` no diretório de configuração do offrig e mantida:
o mesmo projeto recebe a mesma fila após cada reinicialização. A alocação usa um arquivo de bloqueio e
escreve o registro atomicamente, para que dois side-cars iniciados juntos nunca compartilhem uma tag,
alias ou porta. Nenhuma fila pode ser `11434` (a porta do Ollama local): o intervalo começa em
`11500`, e um registro editado para indicar o contrário é rejeitado. Um plano registra sua fila e
seu lançamento, o executor, o watchdog e o desligamento, todos usam essa fila, não a configuração global.

Um side-car sempre corresponde, lista ou interrompe apenas pods com o nome de sua própria fila. A fila de outro
projeto, os pods da fila simples `offrig-<profile>` e qualquer outro pod na conta
são deixados de lado: a verificação de lançamento para um pod ativo solicita apenas
sua própria fila, o desligamento rejeita um pod cujo nome não é o da fila do plano e a verificação de órfãos do túnel mata um `ssh` obsoleto apenas quando seu encaminhamento e seu alias são da própria fila.
Planos criados antes da existência de filas não têm nenhuma fila registrada e continuam a ser executados na fila simples,
de modo que um pod lançado sob o esquema antigo é desligado pelo mesmo plano que o iniciou.

**A própria porta do side-car.** `offrig-mcp` usa o protocolo MCP via stdio. Um driver de shell que o mantém
aberto durante toda a sessão (quando a própria conexão MCP da sessão está inativa) o coloca atrás de
uma porta HTTP de loopback, e essa porta costumava ser um número para toda a máquina (`11439`):
um segundo driver de projeto ou qualquer outro programa poderia usá-lo e o primeiro side-car ficaria
inativo sem dizer nada. O padrão agora é por projeto, da fila do projeto, da mesma forma que
a porta do túnel: o slot da fila `i` (porta do túnel `11500 + 2i`) recebe a porta do side-car `11700 + i`.
O intervalo `11700` a `11763` está acima de todas as portas de túnel e executor que uma fila pode ter
(`11500` a `11627`), a `11435` e `11436` da fila simples e a `11434` do Ollama local,
de modo que uma porta de side-car nunca pode ser uma porta de túnel. Nada novo é armazenado: `lanes.toml` é
inalterado e a porta é derivada da fila. `OFFRIG_SIDECAR_PORT` ainda a substitui; um
valor que não é uma porta, está abaixo de 1024 ou é `11434`, `11435`, `11436` ou qualquer coisa no
intervalo do túnel da fila é rejeitado.

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Com `--check`, uma porta ocupada é um erro que indica a porta e, quando um side-car offrig
responde, o projeto ao qual ele serve: `a porta do side-car 11700 está ocupada: um side-car offrig já está atendendo o projeto <caminho> lá. Pare-o primeiro ou defina OFFRIG_SIDECAR_PORT para uma
porta livre`. A verificação pergunta da mesma forma que o driver já responde (uma solicitação como um projeto
que ninguém atende, o que o driver rejeita antes de tocar em qualquer ferramenta), portanto, não muda nada
em um side-car em execução. `offrig_status` relata a `sidecar_port` da fila.

**Uma fila, um pod ativo.** Uma fila tem um alias SSH e uma porta de túnel, portanto, atende um
pod por vez: um segundo pod na fila (`offrig-<tag>-job` ao lado de `offrig-<tag>-jam`)
reapontaria o alias para si mesmo e enviaria a `offrig_put`, `offrig_exec` e `offrig_get` do primeiro plano para a máquina errada. `offrig_launch`, portanto, rejeita enquanto a fila tiver
um plano aberto ou qualquer pod ativo que possua, com `a fila <tag> tem um pod ativo <nome> (plano <id>);
desligue-o primeiro`, before anything is committed or rented. The plain lane's `offrig up`
e o aplicativo rejeita da mesma forma para um pod de outro perfil (o pod do mesmo perfil ainda é reutilizado). Desligue o primeiro plano e, em seguida, inicie o próximo.

## Níveis

Os perfis residem em `%APPDATA%\offrig\config.toml` (escrito na primeira alteração). Padrões:

| Perfil | GPUs | Modelos | Custo típico |
|---|---|---|---|
| pequeno | 1 × RTX 2000 Ada / A4000 | `qwen3:4b` | aproximadamente US$ 0,25/hora |
| médio | 1 × RTX PRO 6000 (96 GB); A100 ou H100 80 GB, se nenhum estiver livre | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | US$ 2,09/hora (A100, fallback: US$ 1,59) |
| de ponta | 4 × RTX PRO 6000 (384 GB), **SGLang** | Qwen3-Coder-480B AWQ 4 bits (252 GB), cerca de 130 GB restantes para o contexto | US$ 8,36/hora |
| de ponta-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 GB): o caminho do mecanismo de ponta, ensaiado de forma econômica | aproximadamente US$ 1,70/hora |
| de ponta-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 GB): os kernels MoE de 4 bits do mecanismo de ponta, ensaiados de forma econômica | aproximadamente US$ 1,70/hora |
| trabalho | 1 × RTX PRO 6000 (96 GB); A100 ou H100 80 GB, se nenhum estiver livre | nenhum: um **pod de trabalho** executa seu trabalho, não um servidor de modelo | US$ 2,09/hora (A100, fallback: US$ 1,59) |
| ensaio | 1 × A40 (48 GB) primeiro; A6000, A5000, 3090, L4 ou 4090, se nenhum estiver livre | nenhum: um **pod de trabalho** para renderizações de canto das sessões de IA (SoulX-Singer) | US$ 0,49/hora (A40) |

Um perfil com uma `recipe` executa um mecanismo diferente do Ollama: uma imagem fixada
(`lmsysorg/sglang:v0.5.20-cu130`), um modelo Hugging Face que ele baixa no início e
argumentos de servidor extras. O offrig define o paralelismo de tensor a partir da contagem de GPUs, o comprimento do contexto a partir do perfil e mantém o mecanismo no loopback do pod; uma receita não pode
substituir esses valores. Para um repositório protegido, `hf_token_secret` indica um segredo RunPod, referenciado como
`{{ RUNPOD_SECRET_<name> }}` para que o token nunca entre nas especificações do pod. O lançamento aguarda
a `/health` do mecanismo e a lista de modelos, relata os pesos no disco enquanto os baixa
e para imediatamente (com o log do mecanismo) se o mecanismo sair.

Cada perfil lista os tipos de GPU em ordem de prioridade; o RunPod usa o primeiro com capacidade.
Quando nenhum estiver livre, um perfil pode aguardar (`wait_for_gpu_minutes`; o de ponta aguarda até 120 minutos):
o offrig verifica a cada minuto e cria o pod no momento em que as GPUs ficam disponíveis. Nada é alugado
enquanto ele aguarda, Ctrl+C ou o Cancelar do aplicativo o interrompe e, se a API de preços do RunPod estiver inativa, ele simplesmente tenta criar novamente a cada minuto. Grandes configurações multi-GPU chegam e saem em minutos.
Os preços são os preços da nuvem segura, lidos ao vivo; a página de preços não é o preço disponível.

### Aguardando capacidade

Um plano com restrições de `no_fallback` ou `max_price_hr` geralmente não atende aos requisitos de capacidade, portanto, as tentativas de execução são repetidas silenciosamente em vez de falharem, sem alocar recursos enquanto isso. O tempo de espera é, em ordem: o `wait_minutes` do plano (um argumento `offrig_plan`, armazenado com o plano; `0` falha imediatamente) ou, caso contrário, o `wait_for_gpu_minutes` do perfil. O perfil `job` tem como padrão 20 minutos. A espera é reduzida para o tempo restante do plano, menos uma reserva de cinco minutos, para que nunca exceda o prazo do plano e, como nenhum recurso é alocado durante a espera, não adiciona nada ao pior cenário previsto. `offrig_launch` relata `capacity_wait_minutes`; enquanto espera, `offrig_job` mostra `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) e uma etapa que indica qual verificação está sendo realizada. Quando a espera termina, a execução falha com `no capacity` e nenhum recurso é alocado.

### Definir o hardware de um plano

Um perfil lista os tipos de GPU em ordem de prioridade e o RunPod seleciona o primeiro que tenha capacidade disponível. Sem limites, um plano pode ser executado em uma placa de fallback com menos memória, um driver mais antigo e um preço diferente. Três limites restringem o plano ao hardware que pode usar. O planejamento ainda é gratuito; os limites apenas restringem o que o plano pode alocar.

| Limite | Onde | Efeito |
|---|---|---|
| `min_cuda` | perfil (`config.toml`) | A versão mais antiga do host CUDA (driver) da lista do RunPod (`13.0`, `12.9`, ... `11.8`). O comando de criação do pod envia todas as versões iguais ou superiores como `allowedCudaVersions`. Para um perfil de tarefa, a versão mais recente entre esta e a versão própria da imagem `[profiles.job] min_cuda` é aplicada. |
| `min_vram_gb` | perfil | A menor quantidade total de VRAM (de todas as GPUs do perfil) que um plano aceita. Ofertas abaixo desse valor são descartadas; um tipo que o RunPod não lista com memória também é descartado. |
| `max_price_hr` | argumento `offrig_plan` | O valor máximo que o pod pode custar, no total de $/hora para todas as suas GPUs (o valor que `offrig_offers` mostra). Ofertas acima desse valor são descartadas, assim como um tipo sem preço listado (não pode ser limitado a um valor máximo). |
| `no_fallback` | argumento `offrig_plan` | Apenas a primeira família de GPU do perfil é permitida. As duas edições RTX PRO 6000 Blackwell (Server e Workstation) são uma família; todas as outras placas, incluindo a A100 SXM e PCIe, são famílias separadas. |

Ambos os campos do perfil são opcionais e têm como padrão não definidos, portanto, um `config.toml` escrito por um offrig anterior é carregado sem alterações. O perfil `job` define `min_cuda = "13.0"`.

`offrig_plan` calcula o pior cenário com base no que resta: `max_hours x min(max_price_hr, the dearest listed price among the remaining GPUs)`. Sem `max_price_hr`, este é o preço mais alto listado no perfil, como antes. Um plano sem recursos restantes é recusado, com o motivo de cada GPU descartada, e nada é escrito.

O plano armazena a lista de GPUs restantes e o limite mínimo do CUDA, e `offrig_launch` aloca recursos apenas a partir deles, nunca da lista completa do perfil. `offrig_job` e o resultado da execução do script auxiliar relatam o tipo de GPU e a versão do CUDA do host que foram realmente alocados, em um objeto `rented`. A API do pod não relata a versão do CUDA do host, portanto, após a conexão SSH ser estabelecida, a execução executa `nvidia-smi` uma vez e lê a versão do cabeçalho (`CUDA Version: 12.8` ou `CUDA UMD Version: 13.4` em drivers mais recentes); `rented.cuda_source` indica `nvidia-smi` ou `pod API`. Se o CUDA do host for mais antigo do que o limite do plano, a GPU não for uma das listadas no plano ou o preço for superior ao do plano, `offrig_job` retorna uma entrada `warnings` e inicia `next_action` com `WARNING`. Nada é encerrado automaticamente: interromper a alocação de recursos é responsabilidade do chamador (`offrig_shutdown`). Se nem a API do pod nem `nvidia-smi` fornecerem uma versão do CUDA, `rented.notes` informa isso e o limite não é verificado.

### Disco do contêiner

Um pod tem dois discos: o disco do contêiner, local ao host, e o volume montado em `/workspace`. Em alguns hosts, `/workspace` é um sistema de arquivos de rede lento: em um pod de tarefa, foram medidos 32 MB/s, em comparação com 354 MB/s no disco do contêiner, e não foi possível buscar cerca de 130 GB de modelos a tempo, enquanto o disco do contêiner tinha apenas 60 GB. O tamanho do disco do contêiner é o `container_disk_gb` do perfil (de 30 a 60 GB nos perfis integrados; o perfil `job` tem 60) e é enviado ao comando de criação do pod como `containerDiskInGb`. `offrig_plan` aceita `container_disk_gb` (de 1 a 2000) para substituir esse valor para um plano; o plano armazena esse valor, a execução o envia e o plano e `offrig_status` mostram o tamanho em vigor.

- O disco do contêiner **não tem custo**: o offrig cobra apenas pelo tempo de uso da GPU, portanto, o pior cenário do plano é o mesmo, independentemente do tamanho. O RunPod cobra pelo disco; não foi verificado se a taxa que ele relata para o pod (`offrig_job` mostra) inclui o disco do contêiner.
- O offrig não move seus downloads automaticamente. Os comandos de tarefa começam em `HF_HOME` no volume `/workspace` (`/workspace/hf`); para usar o disco do contêiner, defina o seu próprio (`HF_HOME=/root/hf python ...`) no comando.
- O disco do contêiner é excluído com o pod, como o volume sem um volume de rede: copie os resultados de volta com `offrig_get` antes de `offrig_shutdown`.
- O limite de 1 a 2000 é uma verificação de sanidade do próprio offrig para evitar erros de digitação; o limite real do RunPod não é verificado.

### Pods de tarefa

Um perfil com um `job` aloca uma GPU para um trabalho que é executado nela, como uma execução de treinamento, em vez de para servir um modelo. O pod executa uma imagem PyTorch definida (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 para Blackwell) com sshd e nada mais:

- Não serve a nenhum modelo, portanto não há túnel e nada está conectado ao Zed. O sshd não permite nenhum tipo de encaminhamento (`AllowTcpForwarding=no`); a única forma de acesso é via ssh para o pod.
- Um perfil de tarefa não lista nenhum modelo e também não pode ter uma receita; a verificação da configuração recusa ambos.
- `offrig up` e o aplicativo recusam um perfil de tarefa antes de alugar qualquer coisa. Ele é executado através do side-car: `offrig_plan profile=job`, `offrig_launch`, depois `offrig_put`, `offrig_exec` e `offrig_get`. O lançamento está pronto quando o sshd responde.
- Um comando é executado de forma independente no pod (`setsid nohup`) em `/workspace/job`, portanto, ele sobrevive ao side-car e à sessão ssh. É enviado como base64, para que nada nele seja lido pelo shell ssh. Seu log e status de saída permanecem em `/workspace/offrig/jobs/`. Os downloads do Hugging Face vão para `/workspace/hf` no volume do pod.
- `offrig_exec action=run` é para verificações rápidas (`ls`, `nvidia-smi`), não para trabalho: ele executa o comando até a conclusão em `timeout` (padrão de 30 s, no máximo 120 s) e retorna `stdout`, `stderr`, `exit_code` e `timed_out`. A saída é cortada para os últimos 64 KB de cada fluxo (`truncated`) e é uma saída de pod não confiável. Um comando que precisa de mais tempo é um `start`.
- O final do log de uma tarefa tem barras de progresso recolhidas: as redesenhos no estilo tqdm, unidos por retornos de carro, mostram apenas seu último quadro. `offrig_exec action=status save_log=<local path>` também copia todo o log da tarefa, conforme escrito, para um arquivo local (as pastas pai são criadas), para que o final possa permanecer curto.
- A imagem é uma compilação CUDA 12.8, portanto, um perfil de tarefa especifica a versão CUDA mais antiga do host em que ele é executado (`min_cuda = "12.8"`) e o pod é criado com o `allowedCudaVersions` da RunPod a partir dele. Sem isso, um host com um driver mais antigo inicia o pod e o torch não encontra nenhuma GPU, depois que o aluguel é iniciado. O próprio trabalho pode precisar de mais do que a imagem: o perfil `job` também define `min_cuda = "13.0"` no perfil (veja acima), porque os trabalhos que ele executa instalam um vLLM atual, cujo PyTorch é uma compilação CUDA 13.
- Orçamento, plano, watchdog e desligamento funcionam como em qualquer outro perfil. Copie os resultados antes de `offrig_shutdown`: o disco do pod vai junto.
- `jam` é o perfil de tarefa que o ai-jam-sessions usa para renderizar suas músicas: o SoulX-Singer precisa de muito menos do que uma placa de treinamento, portanto, ele aluga uma placa barata de 24 a 48 GB. A configuração e a sessão estão nesse repositório (`docs/vocal-offrig.md`); o offrig não sabe nada sobre música.

### Pesos em fase de preparação em um volume de rede

Um perfil de receita baixa seus pesos em cada lançamento: para o modelo de ponta, isso levou cerca de 20 de 22 minutos para estar pronto (252 GB, US$ 8,36/hora). A fase de preparação os coloca em um volume de rede da RunPod uma vez:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- O volume é cobrado mensalmente, independentemente de um pod estar em execução ou não (300 GB para o modelo de ponta custa cerca de US$ 21/mês a US$ 0,07/GB), portanto, apenas um humano faz a fase de preparação; nenhuma ferramenta de agente pode.
- Um volume reside em um único data center, portanto, os pods do perfil são lançados apenas lá, e as ofertas e os planos são precificados lá. Escolha um com armazenamento em rede e as GPUs do perfil; `offrig gpus` e o console da RunPod mostram onde eles estão.
- O download é executado no pod de GPU mais barato disponível naquele data center. O pod é encerrado em caso de sucesso, falha ou tempo limite.
- O volume é registrado no perfil antes do início do download, para que uma fase de preparação com falha nunca seja esquecida; execute novamente para retomar ou `--remove`.
- Um lançamento em fase de preparação executa o Hugging Face offline, apenas quando a fase de preparação é concluída (um marcador no volume). Um volume parcialmente em fase de preparação baixa o restante em vez de falhar.

## Segurança financeira

- Antes de um lançamento, o offrig mostra a correspondência gratuita mais barata e seu tempo de execução com o pod em execução. Com menos de uma hora de tempo de execução, ele se recusa, a menos que você o substitua, porque, quando o tempo chega a zero, a RunPod interrompe todos os pods na conta, incluindo aqueles que o offrig não gerencia.
- O desligamento automático encerra o pod após 30 minutos com todas as GPUs abaixo de 5% (configurável ou desativado).
- Fechar o aplicativo com um pod em execução pergunta se deve encerrá-lo ou mantê-lo em execução.
- O offrig só toca nos pods que ele nomeou: `offrig-<profile>` para a CLI e o aplicativo, `offrig-<tag>-<profile>` para a faixa side-car de um projeto (veja Faixas). Um side-car nunca toca nos pods de outra faixa, na faixa simples ou em qualquer outro pod; esses são listados, nunca alterados.
- Para sessões de agente, o limite é aplicado antes de qualquer gasto: o pior cenário de um plano (preço ao vivo × horas máximas) é confirmado em relação ao orçamento definido pelo usuário e recusado se exceder, e um lançamento recebe apenas um ID de plano, para que um agente não possa definir seu próprio preço.
- Cada lançamento de side-car tem um watchdog que encerra o pod no prazo do plano, e o executor desliga o pod assim que sua fila estiver vazia.

## O que ele altera em sua máquina

| O quê | Onde | Desfazer |
|---|---|---|
| Provedor Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; o primeiro original é mantido como `settings.json.offrig.bak` |
| Modelo padrão do Zed (somente se você pedir) | o mesmo arquivo | `offrig zed-remove` restaura o padrão anterior |
| `OFFRIG_API_KEY` (espaço reservado; o Zed quer uma chave) | ambiente do usuário | `setx OFFRIG_API_KEY ""` ou remova-o nas Propriedades do Sistema |
| Alias SSH `offrig` | `~/.ssh/config`, entre os marcadores `# >>> offrig:offrig >>>` | exclua o bloco marcado |
| Alias SSH `offrig-<tag>`, um por projeto que foi lançado a partir de um side-car | `~/.ssh/config`, entre os marcadores `# >>> offrig:offrig-<tag> >>>` | exclua o bloco marcado |
| Faixas do projeto | `%APPDATA%\offrig\lanes.toml` (caminho do projeto, tag, alias, porta do túnel) | exclua a entrada do projeto enquanto nenhum pod estiver em execução em sua faixa ou o arquivo inteiro |
| Chaves de host do pod | `~/.ssh/known_hosts_offrig` | exclua o arquivo |
| Configurações | `%APPDATA%\offrig\config.toml` | exclua o arquivo |
| Pesos em fase de preparação (somente com `offrig stage --yes`) | um volume de rede da RunPod `offrig-<profile>`; cobra mensalmente | `offrig stage <profile> --remove --yes` |

Comentários e layout nas configurações do Zed são preservados: as edições passam por uma árvore de sintaxe JSONC.

## Modelo de ameaças

- **Chave da API RunPod.** Lida a partir de `RUNPOD_API_KEY`; nunca é gravada em disco ou em logs. O provedor do Zed, de propósito, não se chama `runpod`: com esse nome, o Zed leria `RUNPOD_API_KEY` e a enviaria para o servidor de modelo.
- **Servidor de modelo.** Acessível apenas via SSH com sua chave. O login por senha está desativado no pod, e o sshd permite apenas o encaminhamento local.
- **Chaves de host.** Fixadas por ponto de extremidade em um arquivo separado known-hosts. O offrig esquece uma chave apenas quando o ponto de extremidade do pod muda, porque o RunPod reutiliza pares de ip:porta em diferentes pods.
- **Injeção de shell.** Os nomes dos modelos são validados em relação à sintaxe de nomes do Ollama antes de chegarem a um shell remoto.
- **Túneis órfãos.** Se o offrig falhar, seu `ssh` pode continuar mantendo a porta. No próximo início, o offrig o encerra, mas apenas se o listener estiver `ssh.exe` carregando a especificação de encaminhamento exata do offrig. Qualquer outra coisa na porta é recusada, nunca encerrada.
- **Sem telemetria.** O offrig se comunica apenas com a API do RunPod, seu pod e seu Ollama local (para comparar listas de modelos).

## Testes

`cargo test --workspace` executa mais de 250 testes, cobrindo pelo menos 90% das linhas (o CI falha abaixo disso):

- **A biblioteca principal:** análise do RunPod, especificações de pod para ambos os mecanismos, configuração SSH, edições JSONC do Zed, regras de proteção, lógica de custo e inatividade, o armazenamento e suas migrações, funções, montagem de contexto, verificações determinísticas, as decisões do executor, o watchdog e o staging, incluindo um RunPod simulado que prova que um estágio com falha encerra seu pod.
- **O aplicativo:** gerenciamento de estado mais testes de interface do usuário de clique em egui.
- **A CLI:** códigos de saída, níveis de log e o fato de que a chave da API nunca aparece na saída.
- **O side-car:** teste completo via stdio contra um RunPod simulado, o processo real do watchdog e o processo real do executor contra um modelo de pod simulado; cada erro de ferramenta carrega um código.

`scripts/verify.sh` (ou `scripts/verify.ps1`) executa a verificação de formato, clippy, os testes e uma execução de teste de cada binário em um único comando. O CI também executa `cargo deny`, uma verificação OSV de `Cargo.lock`, cobertura para o Codecov e `atlas check`.

### Registro de teste ao vivo (2026-10-02, nível médio, A100 80 GB, cerca de US$ 0,45)

- O pod é iniciado em cerca de 80 segundos; sshd, túnel e o Ollama 0.35.0 do pod respondem.
- 97 GB de modelos são baixados no pod a cerca de 150–250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` e `gpt-oss:120b` responderam a um chat transmitido com uma chamada de ferramenta correta através do túnel. Eles usaram 36 GB e 64 GB da VRAM do pod; o Ollama local não carregou nada e não estava na GPU local.
- Todas as sete verificações de proteção foram aprovadas, tanto pela CLI quanto pelo aplicativo.
- Uma CLI encerrada abruptamente deixou seu `ssh` mantendo a porta; a próxima execução a recuperou.
- O túnel, as verificações, o teste de modelo e o desligamento do aplicativo foram executados através de seus botões.

Bugs encontrados durante a execução ao vivo, agora corrigidos e cobertos: uma lista `&&` em segundo plano manteve o stdout do ssh aberto e travou o início do download; a lista de pods não tinha tipos de GPU sem `includeMachine=true`; a verificação de lançamento contou o preço de um pod em execução duas vezes.

### Ensaio do side-car (2026-10-03, nível pequeno, RTX 2000 Ada, US$ 0,08 reservado)

O `offrig-mcp` instalado é executado via stdio, da maneira que um agente o chama:

- `offrig_plan` calculou 0,5 h a US$ 0,15 no pior caso; `offrig_launch` confirmou, iniciou o watchdog e uma segunda chamada retornou o mesmo trabalho. Um pod foi alugado, a US$ 0,24/h.
- SSH foi iniciado 100 s após o aluguel, `qwen3:4b` baixado, pronto em 150 s.
- `offrig_ask` executou uma transferência de designer de jogos em 46 s; a resposta atendeu à sua verificação de aceitação e manteve a restrição de cinco itens da memória.
- O pod acessou a internet (Wikipedia, API do GitHub). Dados os dados fornecidos, o pod respondeu corretamente a perguntas atuais; quando perguntado sem contexto, disse que não tinha acesso ao vivo.
- `offrig_shutdown` de um processo side-car recém-criado encerrou o pod e fechou as contas; o watchdog viu o plano ser encerrado e saiu. A GPU local permaneceu inativa durante todo o processo.

Encontrado e corrigido: o pod atendeu a uma solicitação por vez (`OLLAMA_NUM_PARALLEL=1`); quatro slots receberam 8 solicitações paralelas de 40 a 102 tok/s na mesma GPU, então cada perfil agora tem `parallel = 4`. `complete` foi recusado sem motivo (agora ele assume por padrão "verificação de aceitação atendida"; as falhas ainda precisam de um motivo). O status sugeriu registrar brevemente enquanto uma sessão estava ativa. O texto que vaza para uma resposta é removido, e uma resposta esvaziada pelo pensamento indica que deve-se aumentar `max_tokens`.

### Ensaio do executor (2026-10-03, nível pequeno, RTX 2000 Ada, US$ 0,04 reservado)

Cinco transferências, uma dependendo da outra, funcionaram por `offrig_run` sem ninguém as controlando:

- Quatro transferências em andamento ao mesmo tempo em quatro slots (12,9 GB de 16 GB de VRAM); a dependente começou no momento em que sua dependência foi concluída e foi construída com base em seu resultado.
- As três transferências cujas verificações cobriram a aceitação foram concluídas por conta própria; as histórias de fundo rivais (verificações parciais) e o folclore (sem verificações) foram para revisão.
- A revisão enviou o folclore de volta ("o rio recebeu o nome do projeto"); o executor ao vivo o adotou e revisou com base no feedback ("Rio Veyl").
- A fila foi esgotada em 6,5 minutos (6 turnos, 20.861 tokens); o executor desligou o pod sozinho.

Aprendido: as verificações determinísticas verificam a estrutura, não a qualidade do design. O modelo de 4B passou por "três verbos" com verbos fracos, então `accept_on_checks` é para trabalho estrutural e o trabalho de design vai para revisão. qwen3:4b gastou cerca de 4.000 tokens pensando por turno, mesmo em três linhas de folclore. Uma fila mantém cada slot ocupado apenas quando contém transferências independentes suficientes; uma cadeia de dependência é executada uma de cada vez.

### Execuções de Frontier e SGLang (2026-10-03, US$ 4,27 reservado)

| Executar | Pod | Pronto após | Fila | Reservado |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 min | 4 transferências em 15 s | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 min | 4 transferências | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 min** (252 GB a 278 MB/s, seguido de carregamento) | **31 transferências em 20 s** | **$3.59** |
| Varredura de 30 bilhões de parâmetros | 1 × RTX PRO 6000 | 10 min (colocação lenta de pods) | Apenas varredura | $0.43 |

- SGLang v0.5.20 (cu130) é executado em Blackwell: inferência rápida, `awq_marlin` para os
pesos MoE de 4 bits, paralelismo de tensores via PCIe em quatro placas; `/dev/shm` era 352 GB.
- O trabalho da versão mais recente foi claramente melhor do que o dos modelos menores: respostas no contexto, e um
módulo Rust que foi compilado e passou em seus três testes (verificado localmente). A versão de 30 bilhões de parâmetros FP8
da mesma tarefa não foi compilada.
- Um modelo carregado atende a todo um conjunto; não são necessárias cópias. Varredura de concorrência com
respostas de 384 tokens, total de tokens por segundo:

| Agentes | 480 bilhões de parâmetros em 4 GPUs | 30 bilhões de parâmetros AWQ em 1 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

A velocidade por agente diminui à medida que os agentes são adicionados (480 bilhões: 88 → 24 tok/s em 64), mas o
desempenho total continua a aumentar; os ganhos de 480 bilhões se estabilizam após 256. O cache KV da versão mais recente
contém 398.526 tokens, portanto, com contextos de transferência reais de 2 a 8 mil tokens, a versão mais recente agora executa 64
em paralelo e as versões SGLang de uma única placa executam 32.

Encontrado e corrigido ao longo do caminho: revisões que preenchem a saída para passar em uma verificação de cabeçalho (agora uma
verificação `no_repeats` integrada, e as revisões reestruturam no local); a saída necessária de um determinado papel vazou para os resultados
(a transferência agora define o formato); o feedback do cabeçalho nomeia o formato Markdown; uma revisão inalterada para em vez de repetir.

Ainda não verificado em tempo real: um chat
enviado diretamente do painel do agente Zed (o formato da solicitação que Zed usa é testado diretamente).

## Conformidade com os padrões

Pontuação em relação aos padrões de fluxo de trabalho do estúdio (0 ausentes, 1 parcial, 2 presentes,
3 exemplares).

- **PIN_PER_STEP: 2.** As imagens do pod são fixadas em tags de versão (`ollama/ollama:0.35.0`,
`lmsysorg/sglang:v0.5.20-cu130`; uma receita recusa `latest`), o compilador para 1.98.1,
as dependências por `Cargo.lock` e o mecanismo Atlas para 1.24.0 da frota. Cada etapa de transferência
registra seu modelo, hash de papel e hash de prompt. Os modelos são fixados por tag ou ID de repositório,
não por hash.
- **ANDON_AUTHORITY: 3.** Cada etapa interrompe a execução em caso de defeito: um plano cujos pesos
excedem o disco é rejeitado antes de qualquer gasto; uma extração com falha interrompe o lançamento; uma edição Zed
que não pode ser lida não é gravada; um arquivo de configurações com problemas é relatado, nunca
reescrito; o CI bloqueia em fmt, clippy, testes, licenças e avisos.
- **NAMED_COMPENSATORS: 2.** Cada ação irreversível tem um "undo", listado abaixo.
- **DECOMPOSE_BY_SECRETS: 2.** Um módulo para cada coisa que muda por seus próprios motivos:
a API do RunPod (`runpod`), o conteúdo do pod (`spec`), o transporte (`tunnel`,
`remote`), cada arquivo local que o offrig edita (`sshconfig`, `zed`) e as regras (`guard`,
`cost`). As interfaces front-end não contêm lógica além da apresentação.
- **UNCERTAINTY_GATED_HUMANS: 2.** offrig pergunta apenas quando o resultado é caro ou
com perdas: lançamento com menos de uma hora de tempo disponível, encerramento de um pod (com o que é perdido especificado) e
saída com um pod ainda em execução. Duas decisões pertencem apenas a um humano, e nenhuma
ferramenta de agente pode tomá-las: o limite de orçamento e o preparo de um volume, que são cobrados mensalmente.
A saída da transferência que o código não pode verificar aguarda em revisão em vez de ser concluída.
- **EXTERNAL_VERIFIER: n/a.** Sem reivindicações especializadas.

**Compensadores**

| Ação | Desfazer | Estado após o "undo" | Proprietário |
|---|---|---|---|
| Criar um pod (inicia a cobrança) | `offrig down <profile> --yes`, o aplicativo é desligado ou interrompido automaticamente | pod encerrado, cobrança interrompida | o operador que executa o offrig |
| Encerrar um pod | nenhum para seu disco; relançar o perfil e os modelos são extraídos novamente (um volume de rede os mantém) | novo pod, mesmo perfil | o operador |
| Escrever o provedor Zed ou o modelo padrão | `offrig zed-remove`, ou restaurar `settings.json.offrig.bak` | Zed como antes, offrig | o operador |
| Definir `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` ou excluí-lo nas Propriedades do Sistema | variável removida | o operador |
| Escrever o alias SSH | excluir o bloco marcado em `~/.ssh/config` | configuração como antes | o operador |
| Alocar uma faixa de projeto (o primeiro `offrig_plan` do projeto) | excluir a entrada do projeto de `lanes.toml` quando nenhum pod estiver em execução em sua faixa; um novo plano aloca novamente | faixa livre para reutilização; o bloco de alias é separado (linha acima) | o operador |
| Escrever um bloco de alias SSH da faixa (um lançamento de side-car) | `offrig_shutdown` o remove quando nomeia o pod do plano (também após um lançamento com falha); caso contrário, excluir o bloco `# >>> offrig:offrig-<tag> >>>` em `~/.ssh/config` | configuração como antes; os blocos de outras faixas não são afetados | o operador |
| Extrair um modelo no pod | `ollama rm <model>` no pod, ou encerrar o pod | modelo removido | o operador |
| Encerrar um túnel offrig órfão | nenhum necessário; apenas um `ssh` com o alias e o encaminhamento exatos desta faixa é encerrado, nunca de outra faixa | porta livre | offrig |
| Lançamento de side-car (`offrig_launch`) | `offrig_shutdown`; automático se a configuração falhar; o watchdog no prazo final; o executor quando sua fila esvaziar | pod encerrado, plano fechado com gasto medido | o agente que o chamou, com o watchdog como backup |
| Preparar um volume (`offrig stage --yes`, cobrança mensal) | `offrig stage <profile> --remove --yes` | volume excluído, perfil retorna para o download | o humano que o preparou |
| Iniciar um trabalho em um pod de trabalho (`offrig_exec action=start`) | `offrig_exec action=stop`, ou `offrig_shutdown` | trabalho interrompido com tudo o que ele iniciou; seu log permanece até que o pod seja removido | o agente que o chamou |
| Executar um comando curto em um pod de trabalho (`offrig_exec action=run`) | apenas o que o próprio comando faz; encerrado por `timeout` no máximo em 120 s, ou por `offrig_shutdown` | o pod como o comando o deixou | o agente que o chamou |
| Copie arquivos para ou de um pod de tarefas (`offrig_put`, `offrig_get`) | exclua a cópia (no pod, `offrig_exec`; aqui, o arquivo) | como antes da cópia | o agente que o chamou |

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

## Licença

MIT. Consulte [LICENSE](LICENSE).

---

Criado por <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>
