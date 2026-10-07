<p align="center">
  <a href="README.ja.md">日本語</a> | <a href="README.md">English</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.hi.md">हिन्दी</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (BR)</a>
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

在租赁的 RunPod GPU 上运行大型模型，并保证：这些模型绝不会在您自己的 GPU 上运行。一个桌面应用程序、一个命令行界面和一个用于代理的 MCP 辅助进程，所有这些都基于一个 Rust 库。

该辅助进程允许代理在一个用户设定的预算下规划一个付费会话，租用 GPU，并将一个包含一系列任务的队列交给一个独立的运行器。该运行器会使每个模型插槽保持繁忙状态，仅针对失败的检查进行修订，并在队列为空时关闭 pod。即使其他所有内容都已停止，看门狗也会在计划的截止日期终止 pod。

## 状态

已于 2026-10-02 和 2026-10-03 进行了实际测试，总成本约为 5 美元：

- **前沿（Frontier）：** 4 个 RTX PRO 6000（384 GB），在 SGLang 上运行 Qwen3-Coder-480B（4 位 AWQ），准备时间为 22 分钟，然后 31 个任务在 20 秒内完成，成本为 3.59 美元。
- **集群数据：** 一个加载的模型可以同时为数百个代理提供服务。480B 在 512 个代理下达到 4,059 个 token/秒；一个 30B 模型在一张卡上达到 10,147 个 token/秒，代理数为 256。
- **运行器：** 一个包含依赖关系和重新提交的队列，在没有人工干预的情况下运行，pod 在任务完成后关闭。
- **保证：** 在所有运行过程中，本地 GPU 始终处于空闲状态。

自 2026-10-07 起，两个项目同时使用，每个项目都在自己的环境中运行：在 `job` 个 pod 上运行 aspire-si 的训练任务，以及在 `jam` 个 pod 上运行 ai-jam-sessions 的渲染任务。

已构建和测试，正在等待决策：将前沿权重部署到网络卷上，每月成本约为 21 美元（参见[将权重部署到网络卷](#staging-weights-on-a-network-volume)）。

下一步：第一个真正的“前沿”队列，在启动前完全规划；代码任务编译并在 pod 上进行测试。

## 它的作用

从一个窗口（或一个命令）开始，offrig：

1. 显示您的 RunPod 余额、实时 GPU 价格以及余额可以使用多长时间；
2. 启动一个 pod，用于一个层级，从 Ollama 上的 1 张小型卡到 SGLang 上的 4 × RTX PRO 6000，并在 pod 上加载其模型；
3. 打开到 pod 的 SSH 隧道；
4. 将 pod 的模型添加到 Zed 中，作为其自身的提供程序；
5. 运行七个检查，以确保模型不会加载到本机上；
6. 关闭 pod，或在所有 GPU 都处于空闲状态一段时间后终止它。

通过辅助进程，代理还可以根据预算规划会话，在压缩和重新启动期间保留项目内存，并运行无人值守的任务队列（参见[用于代理的辅助进程](#the-side-car-for-agents)）。

## 保证，以及如何维持

- **模型服务器只能通过隧道访问。** pod 运行一个固定的引擎，Ollama（`ollama/ollama:0.35.0`）或 SGLang（`lmsysorg/sglang:v0.5.20-cu130`），绑定到 pod 自身的环回地址，并且 pod 仅公开 `22/tcp`。没有公共 HTTP 端点可以找到或滥用。无法通过配方将引擎移出环回地址。
- **Zed 通过隧道进行通信，使用其自身的端口。** 隧道监听 `127.0.0.1:11435`。您的本地 Ollama 位于 `11434`。offrig 拒绝将隧道放在 `11434` 上，因此死隧道无法连接到本地服务器：请求将失败。
- **Zed 绝不会切换提供程序。** pod 的模型是 Zed 中一个单独的 `offrig` 提供程序。如果 pod 停止运行，选择其中一个模型会出错；Zed 不会尝试另一个提供程序。
- **权重绝不会存在于本地。** 模型在 pod 上加载，由 pod 加载（或从 Hugging Face 下载到那里，或从已部署的网络卷中读取）。

守卫检查每次都会验证这一点，基于 offrig 可以观察到的事实：

| 检查 | 失败时 |
|---|---|
| 隧道避免使用本地 Ollama 端口 | 隧道端口为 11434 |
| Zed 通过隧道发送 pod 模型 | Zed 的提供程序 URL 不是隧道 |
| Pod 的 Ollama 没有暴露到互联网 | pod 将端口 11434 公开 |
| 隧道指向 pod | 通过隧道读取的模型列表与通过 SSH 在 pod 上读取的列表不同 |
| Pod 模型不在本机上 | pod 模型也存在于本地 Ollama 中 |
| 没有 pod 模型与本地 Zed 模型共享名称 | offrig 提供程序中的名称也存在于 Zed 的本地 Ollama 列表中 |
| Zed 提供的每个模型都在 pod 上 | Zed 提供了一个 pod 没有的模型 |

## 安装

需要安装了 OpenSSH（内置）的 Windows 系统，如果您希望在编辑器中使用模型，则需要安装 Zed，以及一个 RunPod 帐户。

1. 从[发布](https://github.com/mcp-tool-shop-org/offrig/releases)下载 `offrig-<version>-windows-x64.zip`，将其与发布的 `SHA256SUMS` 进行检查，然后将其解压缩到您的 `PATH`。它包含 `offrig.exe`（命令行界面）、`offrig-app.exe`（应用程序）和 `offrig-mcp.exe`（辅助进程）。如果要从源代码构建，请执行：`cargo build --release`，需要 Rust 1.98.1（在 `rust-toolchain.toml` 中固定）。
2. 将您的 RunPod API 密钥放入用户环境变量 `RUNPOD_API_KEY` 中。
3. 在 RunPod 的帐户设置中添加您的 SSH 公钥。offrig 使用 `~/.ssh/runpod_rustline`（如果存在），然后使用 `~/.ssh/id_ed25519`。
4. 对于代理，请在用户范围内将辅助进程注册到 Claude Code：`claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`。它仅在首次使用时打开一个项目的存储，因此对于从未使用的项目来说，它是无害的。

[手册](https://mcp-tool-shop-org.github.io/offrig/handbook/) 介绍了第一个 pod、辅助进程、配置、环境和作业 pod。

## 使用

**应用程序：** 启动 `offrig-app`，选择一个配置文件，然后按“启动 pod”。准备好后，模型将出现在 Zed 的代理面板中，显示为“RunPod · …”。在第一次启动后，重新启动 Zed 一次，以便它可以看到 `OFFRIG_API_KEY`。

**命令行界面：**

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

### 输出、退出代码和错误

- **日志级别：** `-q` 仅打印错误和命令的自身结果；`-v` 添加每次 RunPod 调用及其时间；`--debug` 添加失败的响应主体和完整的错误链。在每个级别都会对 API 密钥进行脱敏处理。
- **退出代码：** `0` 成功，`1` 需要在您的端进行修复（参数、配置、防护或预算拒绝、缺少密钥），`2` 运行时失败（RunPod、网络、ssh、超时、无可用资源）。
- **辅助进程错误**是结果，而不是协议错误：`ok:false` 具有稳定的 `code`、`error` 文本、一个 `next_action` 和 `retryable`。这些代码列在 [手册参考](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/) 中。

## 辅助进程（用于代理）

`offrig-mcp` 是一个 MCP 服务器，代理（例如 Claude Code）将其作为工具调用。它为每个项目在 `<project>/.offrig/offrig.db` 中维护一个数据库，该数据库的生命周期超过每个 Pod，因此会话可以在压缩或重启后继续，而无需重新解释任何内容。

| 工具 | 它的作用 |
|---|---|
| `offrig_status` | 项目、预算、RunPod 余额和剩余时间、offrig 的 Pod，每个开放计划都有其 `plan_id`、通道和 Pod 名称，以及带有标记的待处理队列。 |
| `offrig_offers` | 针对 GPU 数量的实时 GPU 报价 |
| `offrig_plan` | 以最坏情况（实时价格 x 最大小时数）为价格计算会话；如果超出剩余预算，则拒绝。回复中会说明通道的 `ssh_alias` 以及启动将创建的 `pod_name`，以及它将请求的 `container_disk_gb`（可选的 `container_disk_gb` 覆盖配置文件；请参阅“容器磁盘”）。可选的 `max_price_hr` 和 `no_fallback` 限制了它可以租用的 GPU（请参阅“固定计划的硬件”）；可选的 `wait_minutes` 设置了在没有可用资源时启动重试的次数（请参阅“等待可用资源”）。 |
| `offrig_memory_search` | 搜索活动项目内存，每个结果都带有来源和日期 |
| `offrig_memory_record` | 添加简短的约束、决策、事实或检查点；更改是具有理由的替代 |
| `offrig_handoffs` | 将角色导向的任务排队（每个任务都需要进行验收检查；可选的确定性检查），列出这些任务，预览角色块，显示任务的最佳输出（也写入 `.offrig/out/`），记录结果（完成、无效、违规、失败、带反馈的重试） |
| `offrig_launch` | **消耗。** 仅接受一个 `plan_id`：提交最坏情况，等待 GPU 租用，启动 Pod，打开隧道，拉取模型，启动看门狗。每个计划都是幂等的。如果通道已经有一个活动计划或 Pod，则拒绝：`lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | 启动进度（在 Pod 启动时，步骤是从 Pod 在调用那一刻的状态中推导出来的；每次容量重试都在 `progress.capacity_wait` 中进行计数），实际租用的 GPU 类型和主机 CUDA 版本（在 Pod 上使用 `nvidia-smi` 进行测量，当主机比计划的 CUDA 下限旧时，会发出响亮的 `warnings` 条目），看门狗的存活状态，剩余分钟数，到目前为止的消耗 |
| `offrig_ask` | Pod 模型上的一个任务，上下文是从项目存储中构建的；回复将作为不受信任的输出返回 |
| `offrig_run` | 启动一个分离的运行程序，该运行程序会使每个模型插槽保持繁忙：草拟每个准备好的任务，最多针对失败的检查进行两次修订，将结果提供给相关的任务，然后在队列为空时关闭 Pod（除非 `keep_pod`）。代码无法检查的工作将在审核中等待 |
| `offrig_put` | 将本地文件或目录复制到作业 Pod（scp）；相对 Pod 路径位于 `/workspace/job` 下。可选的 `plan_id`（参见下文） |
| `offrig_exec` | 在作业 Pod 上运行一个 bash 命令，以分离的方式运行，因此其生命周期超过辅助进程（`start`），报告正在运行或已退出，并显示其退出代码和日志尾部（`status`；`save_log` 还会将整个日志复制到本地文件），终止它（`stop`），或者立即运行一个简短的命令并返回其 stdout、stderr 和退出代码（`run`，`timeout_secs` 默认值为 30，最多为 120）。可选的 `plan_id`（参见下文） |
| `offrig_get` | 将文件或目录从作业 Pod 复制回，创建缺少的本地父文件夹；在关闭之前执行此操作，关闭操作会删除 Pod 的磁盘。可选的 `plan_id`（参见下文） |
| `offrig_shutdown` | **销毁 Pod。** 终止它，并使用测量的消耗来关闭计划的账本；如果任务正在进行中，则拒绝，除非提供理由。当它命名该 Pod 时（`ssh_block_removed`），它会删除通道自己的 `~/.ssh/config` 块。 |

**作业工具作用于哪个作业计划。** `offrig_put`、`offrig_exec` 和 `offrig_get` 接受一个可选的 `plan_id`。如果只有一个开放的作业计划且没有 `plan_id`，则它们会像以前一样使用它。如果存在多个开放的作业计划且没有 `plan_id`，则它们会拒绝并列出开放的计划（ID、配置文件、Pod 名称）；它们绝不会猜测。如果提供 `plan_id`，则它们仅对该计划的 Pod 进行操作，并且仅在检查 Pod 的名称是否为该计划拥有的 Pod 之后（如果通道的另一个 Pod 被拒绝，则不仅仅是另一个通道的 Pod）。每个作业工具的回复以及 `offrig_job` 都说明了它所作用的 `project` 和 `plan_id`（作业工具回复还说明了 `lane`）；`offrig_status` 说明了 `project` 并列出了每个开放的计划及其 `plan_id`、通道和 Pod 名称。

角色来自 Role OS（档案和启动包卡），以及此处以 Role OS 格式提供的四种游戏角色：游戏设计师、系统设计师、叙事设计师、故事保管者。预算上限仅由人类设定：

```text
offrig budget 15          set this project's cap (run in the project directory)
offrig budget             show cap, committed, spent, remaining
```

每个启动都会启动一个**看门狗**：一个单独的进程，即使代理、会话或辅助进程已消失，也会在计划的截止日期（承诺的时间 + 最大小时数）终止 Pod。它绝不会对失败的查找采取行动，它会精确地执行一次，关闭账本，并记录到 `.offrig/watchdog-<plan>.log`。如果准备好租用的 Pod 失败，启动将终止它，而不是让它继续计费。

设计及其证据位于 [docs/sidecar-design.md](docs/sidecar-design.md)。

### 通道：每个项目一个辅助进程，没有冲突

两个项目可以同时在一个 RunPod 帐户上运行辅助进程。每个项目都有自己的**通道**：一个 SSH 别名、一个隧道端口和一个 Pod 名称标签，这些标签不会与其他项目共享。

| | 普通通道（CLI、应用程序、Zed） | 项目的通道 |
|---|---|---|
| SSH 别名 | `offrig` | `offrig-<tag>` |
| 隧道端口 | `11435`（运行程序 `11436`） | 第一个空闲的 `11500`、`11502`、...（运行程序：上面的端口） |
| 辅助进程端口（shell 驱动程序） | 无 | `11700` + 通道的插槽：`11700`、`11701`、... |
| Pod 名称 | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| SSH 块 | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` 来自项目文件夹的名称（`aspire-si`、`ai-jam-sessions`），并在两个项目共享文件夹名称时添加一个简短的哈希值。项目的通道在首次计划会话时分配，写入 offrig 的配置目录中的 `lanes.toml`，并保留：每次重启后，同一项目都会获得相同的通道。分配需要一个锁文件，并以原子方式写入注册表，因此，同时启动的两个 side-car 进程绝不会共享标签、别名或端口。没有通道可以设置为 `11434`（本地 Ollama 的端口）：范围从 `11500` 开始，并且如果编辑注册表以指定其他值，则会被拒绝。计划会记录其通道及其启动，runner、watchdog 和 shutdown 都会使用该通道，而不是全局配置。

side-car 进程只会匹配、列出或停止为其自身通道命名的 pod。另一个项目的通道、普通通道的 `offrig-<profile>` pod 以及帐户上的任何其他 pod 都会被忽略：用于检查活动 pod 的启动检查仅检查其自身通道，shutdown 拒绝名称不是计划通道的 pod，并且 tunnel 的孤立检查仅在它的转发和别名是通道自身时才会终止过时的 `ssh`。在通道存在之前创建的计划不会记录任何通道，并继续在普通通道上运行，因此，在旧方案下启动的 pod 将由启动它的同一计划关闭。

**side-car 进程自身的端口。** `offrig-mcp` 通过 stdio 传输 MCP。一个 shell 驱动程序会在整个会话期间保持一个连接（当会话自身的 MCP 连接出现问题时），并通过一个环回 HTTP 端口将其置于其后，并且该端口曾经是整个机器的一个数字（`11439`）：第二个项目的驱动程序或任何其他程序都可以占用它，并且第一个 side-car 进程会默默地停止。现在默认设置为每个项目，来自项目的通道，就像 tunnel 端口一样：通道插槽 `i`（tunnel 端口 `11500 + 2i`）获得 side-car 端口 `11700 + i`。范围 `11700` 到 `11763` 位于一个通道可以拥有的每个 tunnel 和 runner 端口之上（`11500` 到 `11627`）、普通通道的 `11435` 和 `11436`，以及本地 Ollama 的 `11434`，因此，side-car 端口绝不能是 tunnel 端口。不会存储任何新内容：`lanes.toml` 未更改，并且端口从通道推断得出。`OFFRIG_SIDECAR_PORT` 仍然会覆盖它；如果值不是端口、低于 1024，或者为 `11434`、`11435`、`11436` 或通道 tunnel 范围内的任何值，则会被拒绝。

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

使用 `--check`，一个被占用的端口是一个错误，它会显示端口名称，并且当一个 offrig side-car 进程在那里响应时，它会显示它所服务的项目：`side-car 端口 11700 已被占用：一个 offrig side-car 进程已经在那里为项目 <path> 提供服务。首先停止它，或者将 OFFRIG_SIDECAR_PORT 设置为可用的端口`。检查方式与驱动程序已经响应的方式相同（一个请求，就像一个没有人服务的项目，驱动程序在触及任何工具之前会拒绝它），因此，它不会更改正在运行的 side-car 进程中的任何内容。`offrig_status` 报告通道的 `sidecar_port`。

**一个通道，一个活动 pod。** 一个通道有一个 SSH 别名和一个 tunnel 端口，因此它一次只能服务一个 pod：通道中的第二个 pod（`offrig-<tag>-job` 位于 `offrig-<tag>-jam` 旁边）会将别名重新指向自身，并将第一个计划的 `offrig_put`、`offrig_exec` 和 `offrig_get` 发送到错误的机器。因此，`offrig_launch` 会拒绝，直到通道具有一个打开的计划或它拥有的任何活动 pod，并显示消息：`通道 <tag> 具有一个活动 pod <name>（计划 <id>）；首先关闭它`, before anything is committed or rented. The plain lane's `offrig up`，并且应用程序以相同的方式拒绝：对于另一个配置文件的 pod（同一配置文件的 pod 仍然会被重用）。关闭第一个计划，然后启动下一个。

## 层级

配置文件位于 `%APPDATA%\offrig\config.toml`（首次更改时写入）。默认值：

| 配置文件 | GPU | 模型 | 典型成本 |
|---|---|---|---|
| 小型 | 1 × RTX 2000 Ada / A4000 级别 | `qwen3:4b` | 约 $0.25/小时 |
| 中型 | 1 × RTX PRO 6000（96 GB）；如果没有任何可用的，则使用 A100 或 H100 80 GB | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | $2.09/小时（A100 备用方案 $1.59） |
| 前沿 | 4 × RTX PRO 6000（384 GB），**SGLang** | Qwen3-Coder-480B AWQ 4 位（252 GB），剩余约 130 GB 用于上下文 | $8.36/小时 |
| 前沿迷你 | 1 × RTX PRO 6000，**SGLang** | Qwen3-Coder-30B FP8（31 GB）：前沿引擎路径，以低成本进行预演 | 约 $1.7/小时 |
| 前沿迷你-awq | 1 × RTX PRO 6000，**SGLang** | Qwen3-Coder-30B AWQ（17 GB）：前沿的 4 位 MoE 内核，以低成本进行预演 | 约 $1.7/小时 |
| 作业 | 1 × RTX PRO 6000（96 GB）；如果没有任何可用的，则使用 A100 或 H100 80 GB | 无：一个 **作业 pod** 运行您的工作，而不是模型服务器 | $2.09/小时（A100 备用方案 $1.59） |
| 即兴演奏 | 1 × A40（48 GB）首先；如果没有任何可用的，则使用 A6000、A5000、3090、L4 或 4090 | 无：一个 **作业 pod**，用于 ai-jam-sessions 的歌唱渲染（SoulX-Singer） | $0.49/小时（A40） |

一个具有 `recipe` 的配置文件运行另一个与 Ollama 不同的引擎：一个固定的镜像（`lmsysorg/sglang:v0.5.20-cu130`）、一个 Hugging Face 模型，它在启动时下载，以及额外的服务器参数。offrig 根据 GPU 数量设置张量并行性，根据配置文件设置上下文长度，并将引擎保留在 pod 的环回中；配方不能覆盖这些设置。对于一个受限的存储库，`hf_token_secret` 指定一个 RunPod 密钥，作为 `{{ RUNPOD_SECRET_<name> }}` 引用，因此令牌永远不会进入 pod 规范。启动会等待引擎的 `/health` 和模型列表，在下载时报告磁盘上的权重，并且如果引擎退出，则会立即停止（并显示引擎的日志）。

每个配置文件都按优先级顺序列出 GPU 类型；RunPod 会选择具有可用容量的第一个 GPU。如果没有可用的 GPU，配置文件可以等待（`wait_for_gpu_minutes`；前沿最多等待 120 分钟）：offrig 每分钟检查一次，并在 GPU 释放时创建 pod。在等待期间不会租用任何资源，按 Ctrl+C 或应用程序的“取消启动”会停止它，并且如果 RunPod 的价格 API 无法访问，它会简单地每分钟重试创建。大型多 GPU 设置会在几分钟内出现和消失。价格是安全云的价格，实时读取；定价页面不是可用的价格。

### 等待容量

一个计划如果使用 `no_fallback` 或 `max_price_hr`，则经常会超出容量，因此启动时会尝试重新启动，而不是失败，并且在尝试期间不会租赁任何资源。它等待的时间顺序如下：计划的 `wait_minutes`（一个 `offrig_plan` 参数，与计划一起存储；`0` 会立即失败），否则是配置文件的 `wait_for_gpu_minutes`。 `job` 配置文件的默认值为 20 分钟。等待时间缩短为计划剩余时间减去五分钟的缓冲时间，因此它永远不会超过计划的截止日期，并且由于在等待期间没有租赁任何资源，因此不会增加已承诺的最坏情况。 `offrig_launch` 报告 `capacity_wait_minutes`；在等待期间，`offrig_job` 显示 `progress.capacity_wait`（`checks`、`waited_secs`、`limit_secs`）以及一个步骤，说明这是哪个检查。当等待时间结束时，启动将使用 `no capacity` 失败，并且没有租赁任何资源。

### 固定计划的硬件

配置文件按优先级顺序列出 GPU 类型，RunPod 会选择具有可用容量的第一个 GPU，因此，如果没有限制，计划可能会选择一个备用 GPU，该 GPU 的内存较少、驱动程序较旧且价格不同。三个限制可确保计划使用其工作所需的硬件。计划仍然是免费的；这些限制只是缩小了计划可以租赁的内容。

| 限制 | 位置 | 效果 |
|---|---|---|
| `min_cuda` | 配置文件 (`config.toml`) | RunPod 列表中最旧的主机 CUDA（驱动程序）版本（`13.0`、`12.9`、... `11.8`）。pod 创建会发送所有版本，或者高于该版本的版本，作为 `allowedCudaVersions`。对于作业配置文件，将使用此版本和图像自身的 `[profiles.job] min_cuda` 中较新版本。 |
| `min_vram_gb` | 配置文件 | 计划可以接受的最小总 VRAM（配置文件中所有 GPU 的总和）。低于此值的方案将被拒绝；如果 RunPod 列出的某种类型没有内存，该类型也将被拒绝。 |
| `max_price_hr` | `offrig_plan` 参数 | pod 的最大成本，以所有 GPU 的总 $/hr 为单位（如图 `offrig_offers` 所示）。高于此值的方案将被拒绝，并且如果某种类型当前没有列出价格，该类型也将被拒绝（无法保证其上限）。 |
| `no_fallback` | `offrig_plan` 参数 | 仅允许使用配置文件的第一个 GPU 家族。两个 RTX PRO 6000 Blackwell 版本（服务器版和工作站版）属于一个家族；所有其他卡，包括 A100 SXM 和 PCIe，都属于各自的家族。 |

这两个配置文件字段都是可选的，并且默认情况下未设置，因此，早期 offrig 编写的 `config.toml` 将保持不变。 `job` 配置文件设置 `min_cuda = "13.0"`。

`offrig_plan` 根据剩余内容确定最坏情况：
`max_hours x min(max_price_hr, the dearest listed price among the remaining GPUs)`。如果没有
`max_price_hr`，那么就是配置文件中列出的最昂贵的价格，与之前一样。如果计划中没有剩余资源，则会拒绝该计划，并说明每个被拒绝的 GPU 的原因，并且不会写入任何内容。

计划存储剩余的 GPU 列表和 CUDA 下限，并且 `offrig_launch` 仅从这些 GPU 中进行租赁，而不是从配置文件的完整列表中进行租赁。 `offrig_job` 和 side-car 的启动结果报告实际租赁的 GPU 类型和主机的 CUDA 版本，以 `rented` 对象的形式呈现。pod API 不会报告主机的 CUDA 版本，因此，一旦 ssh 启动，启动将通过该通道运行 `nvidia-smi`，并从标头中读取该版本（`CUDA Version: 12.8`，或
`CUDA UMD Version: 13.4`，对于较新的驱动程序）；`rented.cuda_source` 显示 `nvidia-smi` 或
`pod API`。如果主机的 CUDA 版本低于计划的下限，或者 GPU 不是计划中列出的 GPU，或者价格高于计划的上限，则 `offrig_job` 返回一个 `warnings`
条目，并使用 `WARNING` 启动 `next_action`。不会自动终止任何内容：停止租赁由调用者决定（`offrig_shutdown`）。如果 pod API 或 `nvidia-smi`
都没有提供 CUDA 版本，则 `rented.notes` 会说明这一点，并且不会检查下限。

### 容器磁盘

pod 有两个磁盘：容器磁盘（位于主机本地），以及挂载在
`/workspace` 上的卷。在某些主机上，`/workspace` 是一个缓慢的网络文件系统：一项作业 pod 在此处的测量值为 32 MB/s，而其容器磁盘为 354 MB/s，并且无法及时获取大约 130 GB 的
模型，而容器磁盘只有 60 GB。容器磁盘的大小是配置文件的 `container_disk_gb`（内置配置文件中的 30 到 60 GB；`job` 配置文件具有
60 GB），并且作为 `containerDiskInGb` 传递到 pod 创建中。 `offrig_plan` 接受
`container_disk_gb`（1 到 2000）以覆盖一个计划的设置；计划存储它，启动
发送它，并且计划回复和 `offrig_status` 显示生效的大小。

- 容器磁盘**不收取费用**：offrig 仅收取 GPU 时间费用，因此，无论大小如何，计划的最坏情况都是相同的。RunPod 会收取磁盘费用；它报告的 pod 费用（如图 `offrig_job` 所示）是否包括容器磁盘，这一点尚未在此处进行检查。
- offrig 不会为您移动下载的文件。作业命令从 `/workspace` 卷（`/workspace/hf`）开始，使用 `HF_HOME`；要使用容器磁盘，请在命令中设置自己的
(`HF_HOME=/root/hf python ...`)。
- 容器磁盘将在 pod 终止时被删除，就像没有网络卷的卷一样：
使用 `offrig_get` 在 `offrig_shutdown` 之前将结果复制回去。
- 1 到 2000 的限制是 offrig 自身的合理性检查，以防止出现拼写错误；RunPod 的实际限制未进行检查。

### 作业 pod

具有 `job` 配置文件的 pod 租赁 GPU 以用于在其上运行的工作，例如训练运行，而不是用于提供模型。它的 pod 运行一个固定的 PyTorch 镜像
(`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`，Blackwell 的 CUDA 12.8)
并且只包含 sshd：

- 它不使用任何模型，因此没有隧道，也没有任何内容与 Zed 连接。sshd 不允许任何端口转发（`AllowTcpForwarding=no`）；进入的唯一方式是 SSH 连接到 Pod。
- 一个作业配置文件不包含任何模型，也不能同时包含配方；配置检查会拒绝这两种情况。
- `offrig up`和应用程序在租用任何资源之前都会拒绝使用作业配置文件。它通过以下方式运行：侧边栏：`offrig_plan profile=job`、`offrig_launch`，然后是`offrig_put`、`offrig_exec`和`offrig_get`。当 sshd 响应时，启动就准备好了。
- 一个命令在 Pod（`setsid nohup`）的`/workspace/job`中以分离模式运行，因此它的生命周期比侧边栏和 SSH 会话更长。它以 base64 格式发送，因此 SSH shell 不会读取其中的任何内容。它的日志和退出状态保存在`/workspace/offrig/jobs/`中。Hugging Face 下载会保存到 Pod 卷的`/workspace/hf`中。
- `offrig_exec action=run`用于快速检查（`ls`、`nvidia-smi`），而不是实际工作：它在`timeout`下运行命令，直到完成（默认 30 秒，最多 120 秒），并返回`stdout`、`stderr`、`exit_code`和`timed_out`。输出会被截断到每个流的最后 64 KB（`truncated`），并且是不受信任的 Pod 输出。需要更长时间的命令是一个`start`。
- 作业的日志尾部会折叠进度条：tqdm 样式的重绘通过回车符连接，仅显示其最后一帧。`offrig_exec action=status save_log=<local path>`还会将作业的完整日志（按原样）复制到本地文件（创建父文件夹），以便日志尾部可以保持简短。
- 该镜像是一个 CUDA 12.8 构建，因此作业配置文件会指定它运行的旧est 主机 CUDA 版本（`min_cuda = "12.8"`），并且 Pod 将使用 RunPod 的`allowedCudaVersions`从该版本创建。如果没有这样做，具有较旧驱动程序的宿主机将启动 Pod，并且在租用开始后，torch 将找不到 GPU。实际工作可能需要比镜像更多的资源：`job`配置文件还会在配置文件中设置`min_cuda = "13.0"`（如上所述），因为其中运行的作业会安装一个最新的 vLLM，其 PyTorch 是 CUDA 13 构建。
- 预算、计划、看门狗和关闭功能与任何其他配置文件一样工作。在`offrig_shutdown`之前，将结果复制回去：Pod 的磁盘将随之一起删除。
- `jam`是 ai-jam-sessions 作业配置文件，它用于渲染其歌唱：SoulX-Singer 比训练卡需要的资源少得多，因此它租用了一个廉价的 24-48 GB 卡。设置和会话都位于该仓库（`docs/vocal-offrig.md`）中；offrig 不知道任何关于歌唱的事情。

### 在网络卷上暂存权重

一个配方配置文件会在每次启动时下载其权重：对于之前的版本，大约需要 20 分钟中的 22 分钟才能准备好（252 GB，8.36 美元/小时）。暂存会将它们一次性放置在 RunPod 网络卷上：

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- 无论 Pod 是否运行，该卷都会按月计费（252 GB 的 frontier 大约是 21 美元/月，价格为 0.07 美元/GB），因此只有人类才能进行暂存；任何代理工具都不能。
- 一个卷位于一个数据中心，因此该配置文件的 Pod 随后只能在该数据中心启动，并且会提供和定价。选择一个具有网络存储和配置文件中 GPU 的数据中心；`offrig gpus`和 RunPod 控制台显示它们的位置。
- 下载将在该数据中心中可用的最便宜的 GPU Pod 上运行。Pod 在成功、失败或超时时终止。
- 在下载开始之前，该卷会被记录在配置文件中，因此不会忘记失败的暂存；重新运行以恢复，或使用`--remove`。
- 暂存的启动会离线运行 Hugging Face，仅在暂存完成后（卷上的标记）才会运行。如果卷是半暂存的，它将下载剩余部分，而不是失败。

## 资金安全

- 在启动之前，offrig 会显示最便宜的可用匹配项以及 Pod 运行时您的可用时间。如果可用时间少于一个小时，除非您覆盖，否则它会拒绝，因为当可用时间为零时，RunPod 会停止帐户中的每个 Pod，包括 offrig 不管理的 Pod。
- 自动停止会在每个 GPU 的使用率低于 5% 后 30 分钟内终止 Pod（可配置，或关闭）。
- 在 Pod 运行时关闭应用程序时，会询问是否终止它或继续运行。
- offrig 仅触及它命名的 Pod：`offrig-<profile>`用于 CLI 和应用程序，`offrig-<tag>-<profile>`用于项目的侧边栏通道（请参阅通道）。侧边栏绝不会触及其他通道的 Pod、普通通道的 Pod 或任何其他 Pod；这些 Pod 会被列出，但不会被更改。
- 对于代理会话，在进行任何支出之前会强制执行上限：计划的最坏情况（实时价格 × 最大小时数）会与人类设置的预算进行比较，如果超过预算，则会拒绝，并且启动仅需要一个计划 ID，因此代理无法自行定价。
- 每个侧边栏启动都有一个看门狗，它会在计划的截止日期时终止 Pod，并且运行程序会在队列为空时立即关闭 Pod。

## 它会更改您机器上的哪些内容

| 什么 | 位置 | 撤销 |
|---|---|---|
| Zed 提供程序 `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`；第一个原始文件将保留为 `settings.json.offrig.bak` |
| Zed 默认模型（仅在您要求时） | 相同的文件 | `offrig zed-remove` 恢复之前的默认设置 |
| `OFFRIG_API_KEY`（占位符；Zed 需要一个密钥） | 用户环境 | `setx OFFRIG_API_KEY ""` 或在系统属性中删除它 |
| SSH 别名 `offrig` | `~/.ssh/config`，位于 `# >>> offrig:offrig >>>` 标记之间 | 删除标记的块 |
| SSH 别名 `offrig-<tag>`，每个从侧边栏启动的项目一个 | `~/.ssh/config`，位于 `# >>> offrig:offrig-<tag> >>>` 标记之间 | 删除标记的块 |
| 项目通道 | `%APPDATA%\offrig\lanes.toml`（项目路径、标签、别名、隧道端口） | 在通道中没有 Pod 运行时，删除项目的条目，或者删除整个文件 |
| Pod 主机密钥 | `~/.ssh/known_hosts_offrig` | 删除该文件 |
| 设置 | `%APPDATA%\offrig\config.toml` | 删除该文件 |
| 暂存的权重（仅限 `offrig stage --yes`） | RunPod 网络卷 `offrig-<profile>`；按月计费 | `offrig stage <profile> --remove --yes` |

Zed 设置中的注释和布局将被保留：编辑将通过 JSONC 语法树进行。

## 威胁模型

- **RunPod API 密钥。** 从 `RUNPOD_API_KEY` 读取；绝不会写入磁盘或日志。Zed 提供程序故意不命名为 `runpod`，因为 Zed 会读取 `RUNPOD_API_KEY` 并将其发送到模型服务器。
- **模型服务器。** 只能通过 SSH 访问，使用您的密钥。密码登录已在 pod 上禁用，并且 sshd 仅允许本地转发。
- **主机密钥。** 每个端点都在单独的 known-hosts 文件中进行固定。offrig 仅在 pod 端点发生更改时才会忘记一个密钥，因为 RunPod 在不同的 pod 之间重用 ip:port 对。
- **Shell 注入。** 在模型名称到达远程 shell 之前，会根据 Ollama 的名称语法对其进行验证。
- **孤立的隧道。** 如果 offrig 崩溃，其 `ssh` 可能会继续占用端口。在下一次启动时，offrig 会将其关闭，但前提是侦听器正在承载 offrig 的确切转发规范 `ssh.exe`。端口上的任何其他内容都会被拒绝，绝不会被关闭。
- **无遥测。** offrig 仅与 RunPod 的 API、您的 pod 和您的本地 Ollama（用于比较模型列表）进行通信。

## 测试

`cargo test --workspace` 运行超过 250 个测试，涵盖至少 90% 的代码行（低于此值 CI 将失败）：

- **核心库：** RunPod 解析、两种引擎的 pod 规范、SSH 配置、Zed JSONC 编辑、防护规则、成本和空闲逻辑、存储及其迁移、角色、上下文组装、确定性检查、运行器的决策、看门狗和暂存，包括一个模拟 RunPod，证明失败的阶段会终止其 pod。
- **应用程序：** 状态处理以及 egui 测试框架中的点击式 UI 测试。
- **CLI：** 退出代码、日志级别，以及 API 密钥绝不会出现在输出中。
- **辅助进程：** 通过 stdio 与模拟 RunPod 进行端到端测试、真实的看门狗进程以及真实的运行器进程与模拟 pod 模型进行测试；每个工具错误都会带有一个代码。

`scripts/verify.sh`（或 `scripts/verify.ps1`）运行格式检查、clippy、测试以及每个二进制文件的模拟运行，所有这些都通过一个命令完成。CI 还运行 `cargo deny`，对 `Cargo.lock` 进行 OSV 扫描，并将覆盖率报告到 Codecov 和 `atlas check`。

### 实时测试记录（2026-10-02，中等级别，A100 80GB，大约 0.45 美元）

- Pod 大约在 80 秒内启动；sshd、隧道和 pod 的 Ollama 0.35.0 均已响应。
- 在 pod 上下载了 97 GB 的模型，速度约为 150–250 MB/s。
- `qwen3-coder:30b-a3b-q8_0` 和 `gpt-oss:120b` 都通过隧道响应了一个流式聊天，并进行了正确的工具调用。它们占用了 pod 的 36 GB 和 64 GB 的 VRAM；本地 Ollama 没有加载任何内容，并且根本不在本地 GPU 上。
- 所有七个防护检查均已通过，来自 CLI 和应用程序。
- 突然终止的 CLI 留下了其 `ssh` 占用端口；下一次运行会重新获取它。
- 应用程序的隧道、检查、模型测试和关闭都通过其按钮进行驱动。

实时运行发现并已修复的错误：一个在后台运行的 `&&` 列表会使 ssh 的 stdout 保持打开状态并导致拉取开始时出现问题；pod 列表缺少没有 `includeMachine=true` 的 GPU 类型；启动检查将正在运行的 pod 的价格计算了两次。

### 辅助进程模拟（2026-10-03，小型级别，RTX 2000 Ada，预订 0.08 美元）

通过 stdio 驱动的已安装 `offrig-mcp`，就像代理调用它一样：

- `offrig_plan` 估算了 0.5 小时的成本，最坏情况下为 0.15 美元；`offrig_launch` 提交了它，启动了看门狗，第二次调用返回了相同的工作。租用了一个 pod，价格为 0.24 美元/小时。
- 租用后 100 秒 SSH 启动，`qwen3:4b` 拉取，150 秒后准备就绪。
- `offrig_ask` 在 46 秒内运行了一个游戏设计师交接；回复满足了其验收检查，并保留了内存中的五条约束。
- pod 访问了互联网（Wikipedia、GitHub API）。给定数据后，pod 准确地回答了当前的问题；在没有实时访问的情况下，它表示它没有实时访问权限。
- 来自新的辅助进程的 `offrig_shutdown` 终止了 pod 并关闭了账目；看门狗看到了计划关闭并退出了。本地 GPU 在整个过程中都保持空闲状态。

发现并已修复：pod 一次处理一个请求（`OLLAMA_NUM_PARALLEL=1`）；四个插槽从 40 到 102 tok/s 在同一 GPU 上处理了 8 个并行请求，因此每个配置文件现在都有 `parallel = 4`。如果没有理由，则拒绝 `complete`（现在默认设置为“验收检查已通过”；失败仍然需要一个理由）。状态表明在会话进行时记录了一段时间。正在考虑将泄漏到回复中的文本删除，并且如果回复因思考而为空，则会引发 `max_tokens`。

### 运行器模拟（2026-10-03，小型级别，RTX 2000 Ada，预订 0.04 美元）

五个交接，其中一个依赖于另一个，由 `offrig_run` 驱动，没有人进行控制：

- 四个交接同时在四个插槽中进行（16 GB VRAM 中的 12.9 GB）；当依赖项完成后，依赖项会立即启动，并基于其结果进行构建。
- 三个验收检查涵盖的交接完成了它们自己的工作；竞争对手的背景故事（部分检查）和故事（没有检查）被发送到审查。
- 审查将故事发回（“河流以项目命名”）；实时运行器采用了它，并根据反馈进行了修改（“Veyl 河”）。
- 队列在 6.5 分钟内完成（6 轮，20,861 个令牌）；运行器自行关闭了 pod。

已学习：确定性检查验证结构，而不是设计质量。4B 模型通过了“三个动词”，但动词很薄弱，因此 `accept_on_checks` 用于结构工作，而设计工作则交给审查。qwen3:4b 在每轮中花费了大约 4,000 个令牌进行思考，即使只有三行故事。一个队列仅在其中包含足够的独立交接时，才会使每个插槽保持繁忙；依赖链一次运行一个。

### Frontier 和 SGLang 运行（2026-10-03，预订 4.27 美元）

| 运行 | Pod | 准备就绪后 | 队列 | 预订 |
|---|---|---|---|---|
| frontier-mini（Qwen3-Coder-30B FP8） | 1 × RTX PRO 6000 | 5.5 分钟 | 4 个交接，15 秒 | $0.30 |
| frontier-mini-awq（Qwen3-Coder-30B AWQ） | 1 × RTX PRO 6000 | 4 分钟 | 4 个交接 | $0.25 |
| **frontier（Qwen3-Coder-480B AWQ）** | **4 × RTX PRO 6000** | **22 分钟**（278 MB/s 的 252 GB，然后加载） | **20 秒内完成 31 次切换** | **$3.59** |
| 30B 规模的群组扫描 | 1 × RTX PRO 6000 | 10 分钟（缓慢的 Pod 部署） | 仅扫描 | $0.43 |

- SGLang v0.5.20 (cu130) 运行在 Blackwell 上：flashinfer attention，`awq_marlin` 用于 4 位 MoE 权重，在四个卡上通过 PCIe 进行张量并行；`/dev/shm` 是 352 GB。
- 该模型的表现明显优于小型模型：在模拟环境中的语音输出，以及一个编译并通过了三个测试的 Rust 模块（在本地进行检查）。30B FP8 运行版本的相同任务无法编译。
- 一个已加载的模型服务于整个群组；不需要复制。并发扫描，每个回复包含 384 个令牌，每秒总令牌数：

| 代理 | 480B，在 4 个 GPU 上 | 30B AWQ，在 1 个 GPU 上 |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

随着代理数量的增加，每个代理的速度会降低（480B：88 → 64 时为 24 个令牌/秒），但总吞吐量仍在增加；480B 的性能提升在超过 256 后趋于平缓。该模型的 KV 缓存包含 398,526 个令牌，因此，在实际切换上下文为 2-8k 令牌的情况下，该模型现在可以同时处理 64 个任务，而单卡 SGLang 模型可以同时处理 32 个任务。

在开发过程中发现并修复了以下问题：修订后的输出填充了内容，以通过标题检查（现在是一个内置的 `no_repeats` 检查，并且修订后的内容会进行原地重构）；某个角色的必需输出泄漏到最终交付物中（现在，切换会设置格式）；标题反馈会命名 Markdown 格式；未更改的修订版本会停止，而不是重复。

尚未在实际环境中验证：从 Zed 的代理面板发送的聊天消息（直接测试 Zed 使用的请求格式）。

## 标准合规性

根据工作室的工作流程标准进行评分（0 个缺失项，1 个部分符合项，2 个符合项，3 个优秀项）。

- **PIN_PER_STEP：2。**Pod 镜像固定到版本标签（`ollama/ollama:0.35.0`、`lmsysorg/sglang:v0.5.20-cu130`；一个配方拒绝使用 `latest`），编译器固定到 1.98.1，依赖项通过 `Cargo.lock` 固定，Atlas 引擎固定到集群的 1.24.0。每次切换都会记录其模型、角色哈希和提示哈希。模型通过标签或仓库 ID 进行固定，而不是通过摘要进行固定。
- **ANDON_AUTHORITY：3。**每个步骤都会在出现缺陷时停止运行：如果计划的权重超过磁盘空间，则在任何支出之前会被拒绝；如果拉取失败，则会停止启动；如果 Zed 编辑无法读取，则不会写入；如果设置文件损坏，则会报告，但不会重新写入；CI 会阻止 fmt、clippy、测试、许可证和建议。
- **NAMED_COMPENSATORS：2。**每个不可逆的操作都有一个撤销操作，如下所示。
- **DECOMPOSE_BY_SECRETS：2。**每个模块对应一个会因自身原因而发生变化的事物：RunPod 的 API（`runpod`）、Pod 的内容（`spec`）、传输（`tunnel`、`remote`）、每个本地文件 offrig 编辑（`sshconfig`、`zed`）以及规则（`guard`、`cost`）。前端不包含超出呈现之外的任何逻辑。
- **UNCERTAINTY_GATED_HUMANS：2。**offrig 仅在结果代价高昂或有损时才会询问：在剩余运行时间少于一小时的情况下启动，终止 Pod（并说明损失的内容），以及在 Pod 仍在计费时退出。有两个决定完全由人类做出，并且没有代理工具可以做出这些决定：预算上限和准备卷，后者按月计费。无法通过代码检查的切换输出将等待审核，而不是完成。
- **EXTERNAL_VERIFIER：n/a。**没有专门的声明。

**补偿措施**

| 操作 | 撤销 | 撤销后的状态 | 所有者 |
|---|---|---|---|
| 创建 Pod（开始计费） | `offrig down <profile> --yes`，应用程序的关闭，或自动停止 | Pod 已终止，计费已停止 | 运行 offrig 的操作员 |
| 终止 Pod | 对于其磁盘，没有；重新启动配置文件，并重新拉取模型（网络卷会保留这些模型） | 新的 Pod，相同的配置文件 | 操作员 |
| 写入 Zed 提供程序或默认模型 | `offrig zed-remove`，或恢复 `settings.json.offrig.bak` | offrig 之前的 Zed | 操作员 |
| 设置 `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` 或在系统属性中删除它 | 变量已删除 | 操作员 |
| 写入 SSH 别名 | 删除 `~/.ssh/config` 中标记的块 | 配置与之前相同 | 操作员 |
| 分配项目通道（项目的第一个 `offrig_plan`） | 删除项目在 `lanes.toml` 中的条目，一旦没有 Pod 在其通道中运行；新的计划会再次分配 | 通道可供重用；别名块是单独的（上方的行） | 操作员 |
| 写入通道的 SSH 别名块（一个辅助启动） | `offrig_shutdown` 在命名计划的 Pod 时会删除它（在启动失败后也是如此）；否则，删除 `~/.ssh/config` 中的 `# >>> offrig:offrig-<tag> >>>` 块 | 配置与之前相同；其他通道的块不受影响 | 操作员 |
| 在 Pod 上拉取模型 | `ollama rm <model>` 在 Pod 上，或终止 Pod | 模型已删除 | 操作员 |
| 终止孤立的 offrig 隧道 | 不需要；只有具有此通道的精确别名和转发的 `ssh` 才会终止，绝不会终止其他通道的隧道 | 端口已释放 | offrig |
| 辅助启动（`offrig_launch`） | `offrig_shutdown`；如果设置失败，则自动启动；在截止日期时启动看门狗；当队列耗尽时，启动运行器 | Pod 已终止，计划已关闭，并记录了已发生的支出 | 调用代理，看门狗作为后备 |
| 准备卷（`offrig stage --yes`，按月计费） | `offrig stage <profile> --remove --yes` | 卷已删除，配置文件已恢复到下载状态 | 准备该卷的人 |
| 在作业 Pod 上启动作业（`offrig_exec action=start`） | `offrig_exec action=stop`，或 `offrig_shutdown` | 作业已终止，其启动的所有内容也已终止；其日志会保留，直到 Pod 消失 | 调用代理 |
| 在作业 Pod 上运行一个短命令（`offrig_exec action=run`） | only what the command itself does; ended by `timeout` at most 120 s in, or by `offrig_shutdown` | Pod 处于命令执行后的状态 | 调用代理 |
| 将文件复制到作业 Pod 或从作业 Pod 复制文件（`offrig_put`，`offrig_get`） | 删除副本（在 Pod 上，`offrig_exec`；此处，指文件） | 与复制之前相同 | 调用代理 |

## 布局

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

## 许可

MIT。请参阅 [LICENSE](LICENSE)。

---

由 <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a> 构建。
