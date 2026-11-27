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

[![M8ven Score](https://m8ven.ai/badge/mcp/mcp-tool-shop-org-offrig-790exm?v=3bd60e98b65d7124b74c4126c5ce5ce7)](https://m8ven.ai/mcp/mcp-tool-shop-org-offrig-790exm?s=readme)

在租用的 RunPod GPU 上运行大型模型，并保证：这些模型绝不会在您自己的 GPU 上运行。一个桌面应用程序、一个命令行界面和一个用于代理的 MCP 辅助进程，所有这些都基于一个 Rust 库。

侧边栏允许代理规划一个在人类设定的预算内的付费会话，租用 GPU，并将一个包含角色信息的任务队列交给一个独立的运行程序。该运行程序会使每个模型插槽保持繁忙状态，仅针对失败的检查进行修订，并在队列为空时关闭 Pod。即使其他所有内容都已消失，看门狗也会在计划的截止日期终止 Pod。

## 状态

已于 2026-10-02 和 2026-10-03 成功运行，总计约 5 美元：

- **前沿：** 4 × RTX PRO 6000 (384 GB)，在 SGLang 上运行 Qwen3-Coder-480B (4 位 AWQ)，准备时间为 22 分钟，然后 31 个任务在 20 秒内完成，费用为 3.59 美元。
- **集群数据：** 一个加载的模型可以同时为数百个代理提供服务。480B 模型在 512 个代理的情况下达到 4,059 个令牌/秒；一个 30B 模型在一张卡上达到 10,147 个令牌/秒，代理数量为 256。
- **运行器：** 一个包含依赖关系和重新发送功能的队列，在没有人工干预的情况下运行，Pod 在任务完成后关闭。
- **保证：** 在所有运行过程中，本地 GPU 始终处于空闲状态。

自 2026-10-07 起，两个项目同时使用，每个项目都在自己的通道中：为 aspire-si 在 `job` 个 Pod 上运行训练任务，为 ai-jam-sessions 在 `jam` 个 Pod 上运行音乐渲染任务。

2026-10-11，一个 SGLang Pod 记录了 `Application startup complete`，然后 `/health` 持续 503 错误 7 分钟。GPU 的利用率接近 100%，功耗约为 100 瓦，并且 `/v1/models` 返回了 200。当周正在运行的 Pod 使用的是 CUDA 13.0。该 Pod 被放置在 CUDA 13.2 上。13.2 似乎不是导致问题的原因；该设置允许配置文件保持在已正常运行的版本上。此版本报告了该问题，并且在完成一个任务并返回一个选择之前，不会将该配方标记为已准备好。配置文件还可以精确地设置主机 CUDA 版本。

在本版本中，尚未在租用的 Pod 上进行验证：单令牌就绪检查、`engine_unresponsive` 和 `engine_hung` 报告，以及一个发送 `gpu.allowedCudaVersions` 的创建操作。这些路径通过针对模拟 SSH 会话和模拟 RunPod 的测试来覆盖。没有启动 Pod 来重新检查它们。RunPod 记录的 v1 `POST /pods` 枚举，于 2026-10-11 阅读，仍然停留在 13.0。13.1 和 13.2 包含在 offrig 的列表中，因为该编写 Pod 运行在 13.2 上。一个下限仍然只发送 `gpu.minCudaVersion`。

已构建和测试，正在等待决策：将前沿权重分阶段部署到网络卷上，费用约为每月 21 美元（请参阅 [分阶段部署](#在网络卷上暂存权重)）。

接下来：第一个真正的队列，在启动前完全规划；代码转移编译并在 Pod 上进行测试。

## 它的作用

从一个窗口（或一个命令）开始，offrig：

1. 显示您的 RunPod 余额、实时 GPU 价格以及余额可以使用多长时间；
2. 启动一个分层 Pod，从 Ollama 上的 1 张小型卡到 SGLang 上的 4 × RTX PRO 6000，并在 Pod 上加载其模型；
3. 打开到 Pod 的 SSH 隧道；
4. 将 Pod 的模型添加到 Zed 中，作为其自己的提供程序；
5. 运行七个检查，以确保模型不会加载到本机上；
6. 关闭 Pod，或在所有 GPU 都处于空闲状态一段时间后终止它。

通过辅助进程，代理还可以根据预算规划会话，在压缩和重新启动期间保留项目内存，并以无人值守的方式运行任务队列（请参阅 [辅助进程](#侧边栏用于代理)）。

## 保证，以及如何维持

- **模型服务器只能通过隧道访问。** Pod 运行一个固定的引擎，Ollama (`ollama/ollama:0.35.0`) 或 SGLang (`lmsysorg/sglang:v0.5.20-cu130`)，绑定到 Pod 自己的环回地址，并且 Pod 仅公开 `22/tcp`。没有公共 HTTP 端点可以找到或滥用。配方无法将引擎移出环回地址。
- **Zed 通过隧道与 Pod 通信，使用其自己的端口。** 隧道监听 `127.0.0.1:11435`。您的本地 Ollama 位于 `11434`。offrig 拒绝将隧道放置在 `11434` 上，因此死隧道不会回退到本地服务器：请求将失败。
- **Zed 绝不会切换提供程序。** Pod 的模型是 Zed 中一个单独的 `offrig` 提供程序。如果 Pod 停止运行，选择其中一个模型将导致错误；Zed 不会尝试另一个提供程序。
- **权重绝不会存在于本地。** 模型由 Pod 拉取（或从 Hugging Face 下载，或从分阶段的网络卷读取）。

每次都会进行检查，验证 offrig 可以观察到的事实：

| 检查 | 失败时 |
|---|---|
| 隧道避免使用本地 Ollama 端口 | 隧道端口为 11434 |
| Zed 通过隧道发送 Pod 模型 | Zed 的提供程序 URL 不能是隧道 |
| Pod 的 Ollama 不暴露在互联网上 | Pod 将端口 11434 公开 |
| 隧道在 Pod 处结束 | 通过隧道传输的模型列表与通过 SSH 在 Pod 上读取的列表不同 |
| Pod 模型不在本机上 | Pod 模型也存在于本地 Ollama 中 |
| 没有 Pod 模型与本地 Zed 模型共享名称 | offrig 提供程序中的名称也存在于 Zed 的本地 Ollama 列表中 |
| Zed 提供的每个模型都在 Pod 上 | Zed 提供了一个 Pod 没有的模型 |

## 安装

需要具有 OpenSSH (内置) 的 Windows，如果您希望在编辑器中使用模型，则需要 Zed，以及一个 RunPod 帐户。

1. 从以下位置下载 `offrig-<version>-windows-x64.zip`：
[发布](https://github.com/mcp-tool-shop-org/offrig/releases)，对照发布说明中的 `SHA256SUMS` 进行检查，然后将其解压缩到您的 `PATH` 目录中。其中包含 `offrig.exe`（CLI 命令行工具）、
`offrig-app.exe`（应用程序）和 `offrig-mcp.exe`（辅助进程）。
如果要从源代码进行构建，请执行以下操作：`cargo build --release`，使用 Rust 1.98.1（在 `rust-toolchain.toml` 中指定）。
2. 将您的 RunPod API 密钥放入用户环境变量 `RUNPOD_API_KEY` 中。
3. 在 RunPod 的帐户设置中添加您的 SSH 公钥。offrig 如果存在，则使用 `~/.ssh/runpod_rustline`，否则使用 `~/.ssh/id_ed25519`。
4. 对于代理，请在用户范围内将辅助进程注册到 Claude Code：
`claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`。它仅在首次使用时打开项目的存储，因此在从未使用过的项目中是安全的。

[手册](https://mcp-tool-shop-org.github.io/offrig/handbook/) 介绍了第一个 Pod、辅助进程、配置、通道和作业 Pod。

## 使用

**应用程序：**启动 `offrig-app`，选择一个配置文件，然后按“启动 Pod”。准备就绪后，模型将出现在 Zed 的代理面板中，显示为“RunPod · …”。在首次启动后，重新启动 Zed 一次，以便它能够识别 `OFFRIG_API_KEY`。

**CLI：**

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

`offrig --help` 和 `offrig <command> --help` 列出了所有命令和标志。上面的部分是每日使用的命令集。`offrig init`、`offrig models`、`offrig zed` 和
`offrig up --no-zed` 可以在 [手册](https://mcp-tool-shop-org.github.io/offrig/handbook/usage/) 中找到。

### 输出、退出代码和错误

- **日志级别：**`-q` 仅打印错误和命令的自身结果；`-v` 添加每个 RunPod 调用及其时间；`--debug` 添加失败的响应主体和完整的错误链。
API 密钥在所有级别都会被删除。
- **退出代码：**`0` 表示成功；`1` 表示需要在您的端进行修复（参数、配置、防护或预算拒绝、缺少密钥）；`2` 表示运行时故障（RunPod、网络、ssh、超时、无可用资源）。
- **辅助进程错误**是结果，而不是协议错误：`ok:false`，具有稳定的 `code`、`error` 文本、一个 `next_action` 和 `retryable`。这些代码在 [手册参考](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/) 中列出。

## 侧边栏（用于代理）

`offrig-mcp` 是一个 MCP 服务器，例如 Claude Code 等代理会将其作为工具调用。它为每个项目在 `<project>/.offrig/offrig.db` 处维护一个数据库，该数据库的生命周期长于每个 Pod，因此会话可以在压缩或重启后继续，而无需重新解释任何内容。

| 工具 | 它的作用 |
|---|---|
| `offrig_status` | 项目、预算、RunPod 余额和剩余时间、offrig 的 Pod、每个开放计划及其 `plan_id`、通道和 Pod 名称、带有标记的过时工作的待处理队列。每个 Pod 包含 `gpu_count` 和 `gpus`（`2x <type>`）来自 RunPod 记录，以及计划的 `planned_gpu_count`。 |
| `offrig_offers` | 针对 GPU 数量提供实时的 GPU 资源 |
| `offrig_plan` | 以最坏的情况（实时价格乘以（最大小时数 + 780 秒的自动停止时间），四舍五入到美分）对会话进行定价；如果超出预算，则拒绝。回复中会包含通道的 `ssh_alias` 和启动将创建的 `pod_name`，以及它将请求的 `container_disk_gb`（可选的 `container_disk_gb` 覆盖配置文件；请参阅“容器磁盘”）。可选的 `max_price_hr` 和 `no_fallback` 限制了它可以租用的 GPU（请参阅“固定计划的硬件”）；可选的 `wait_minutes` 设置了在没有可用资源时启动重试的次数（请参阅“等待可用资源”）。 |
| `offrig_memory_search` | 搜索活动项目内存，每个结果都包含来源和日期 |
| `offrig_memory_record` | 添加简短的约束、决策、事实或检查点；更改是具有理由的替代 |
| `offrig_handoffs` | 将包含角色信息的任务排队（每个任务都需要进行验收检查；可选的确定性检查），列出它们，预览角色块，显示任务的最佳输出（也写入 `.offrig/out/`），记录结果（完成、无效、违规、失败、带反馈的重试） |
| `offrig_launch` | **花费。** 仅需要 `plan_id`：提交最坏的情况，等待 GPU 租用，启动 Pod，打开隧道，拉取模型，启动看门狗。每个计划都是幂等的。如果通道中已经存在一个活动计划或 Pod，则拒绝：`lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | 启动进度（在 Pod 启动时，步骤是从 Pod 在调用那一刻的状态派生而来，并且 `progress.pod_status` 是原始的 v2 状态；每次容量重试都会在 `progress.capacity_wait` 中进行计数），GPU 类型、`gpu_count`、`gpus`、`planned_gpu_count`，并且在 ssh 启动后 `measured_gpu_count`（没有卡的读取结果为 `0`）。当记录或测量值与计划不同时，`gpu_mismatch` 为真。使用 `nvidia-smi` 测量主机 CUDA 版本，当主机比计划的 CUDA 下限旧时，会显示一个响亮的 `warnings` 条目。对于配方，`ready` 表示一个完成返回了一个选择。`engine_hung` 和 `engine_unresponsive` 使 Pod 保持启动状态。看门狗的活动状态、剩余分钟数、到目前为止的消耗 |
| `offrig_ask` | Pod 模型上的一个任务，上下文来自项目存储；回复作为不受信任的输出返回 |
| `offrig_run` | 启动一个独立的运行程序，使每个模型插槽保持繁忙状态：草拟每个准备好的任务，最多针对失败的检查进行两次修订，将结果提供给相关的任务，然后在队列为空时关闭 Pod（除非 `keep_pod`）。无法通过代码检查的工作将在审查中等待 |
| `offrig_put` | 将本地文件或目录复制到作业 Pod（scp）；相对 Pod 路径位于 `/workspace/job` 下。可选 `plan_id`（见下文） |
| `offrig_exec` | 在作业 Pod 上运行一个 bash 命令，以独立方式运行，因此其生命周期长于侧边栏（`start`），报告正在运行或已退出，并显示其退出代码和日志尾部（`status`；`save_log` 还会将整个日志复制到本地文件），终止它（`stop`），或者立即运行一个简短的命令并返回其 stdout、stderr 和退出代码（`run`，`timeout_secs` 默认为 30，最多 120）。可选 `plan_id`（见下文） |
| `offrig_get` | 将文件或目录从作业 Pod 复制回，创建缺少的本地父文件夹；在关闭之前执行此操作，关闭后会删除 Pod 的磁盘。可选 `plan_id`（见下文） |
| `offrig_shutdown` | **销毁 Pod。** 终止它并关闭计划的账本，记录已花费的费用；如果任务正在进行中，则拒绝，除非提供理由。当通道的 Pod 命名时，删除通道自身的 `~/.ssh/config` 块（`ssh_block_removed`） |
| `offrig_complete` | **消耗。**仅采用 OpenRouter：一个聊天完成，仅针对允许的项目和模型。提交最坏的情况，以应对预算，将答案写入项目文件（如果该路径已经存在，则替换它），并记录实际费用。 |

**作业工具作用于哪个作业计划。** `offrig_put`、`offrig_exec` 和 `offrig_get` 接受一个可选的 `plan_id`。如果只有一个开放的作业计划且没有 `plan_id`，则使用它，如前所述。如果存在多个开放的作业计划且没有 `plan_id`，则拒绝并列出开放的计划（ID、配置文件、Pod 名称）；它们绝不会猜测。如果存在 `plan_id`，则仅对该计划的 Pod 进行操作，并且仅在检查 Pod 的名称与该计划拥有的 Pod 匹配后才进行操作（通道中的另一个 Pod 将被拒绝，而不仅仅是另一个通道中的 Pod）。每个作业工具的回复以及 `offrig_job` 都将说明它作用于 `project` 和 `plan_id`（作业工具的回复还包括 `lane`）；`offrig_status` 将说明 `project` 并列出每个开放的计划及其 `plan_id`、通道和 Pod 名称。

**任务与技能。** 支持 MCP Tasks 的客户端可以把 `offrig_launch` 作为任务运行：`tasks/get` 显示与 `offrig_job` 相同的状态，`tasks/cancel` 取消仍在进行中的启动（通过 `offrig_shutdown`）；要停止已就绪的会话，请调用 `offrig_shutdown`。何时停止 pod 仍由计划的期限和 watchdog 决定。服务器还发布一个流程技能 `offrig-session`。详情见 [side-car 手册](https://mcp-tool-shop-org.github.io/offrig/handbook/side-car/#tasks)。

角色来自 Role OS（档案和启动包卡），以及在此处以 Role OS 格式提供的四个游戏角色：游戏设计师、系统设计师、叙事设计师、故事保管者。预算上限仅由人类设定：

```text
offrig budget --provider runpod 30       RunPod's cap (run in the project directory)
offrig budget --provider openrouter 8    OpenRouter's cap
offrig budget --show                     every cap, beside each provider's own balance
```

每个 sidecar 启动都会启动一个 **看门狗**：一个独立的进程，即使代理、会话或 sidecar 已经停止，它也会在计划的截止日期（承诺时间 + 最长小时数）终止 pod。它不会对失败的查找操作采取任何措施，只会执行一次终止操作，关闭账目，并将日志记录到 `.offrig/watchdog-<plan>.log`。如果准备好租用的 pod 在引擎开始提供服务之前失败，sidecar 将会终止它，而不是让它继续计费。
`engine_hung` 和 `engine_unresponsive` 在所有界面上仅用于报告：sidecar、CLI 和应用程序会保持 pod 运行，并且不会打印“已就绪”。在 CLI 和应用程序中，命令会出错并停止。在 sidecar 中，启动结果将保持“成功”状态，因此不会删除 pod，计划将继续执行，并且作业将保持 `running` 状态。已经退出的引擎进程不会被报告。在 sidecar 中，这是一个失败的启动，补偿器会删除 pod。使用 `offrig up` 启动的 pod 或应用程序没有看门狗。当命令因错误而停止（例如，挂起、引擎无响应、引擎退出或超时）时，pod 将继续计费，直到执行 `offrig down <profile> --yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED`，此时会在所有界面上终止该 pod。

设计及其证据位于 [docs/sidecar-design.md](docs/sidecar-design.md)。

### 项目索引和验证器

```text
offrig index src docs                 chunk this project; only changed files are re-embedded
offrig index --status                 counts, model, dimension; changes nothing
offrig index --rebuild                drop every vector and embed again
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on
offrig verify calibrate --think-policy grounded=off,reasoning=escalate:off>on --model gemma4:31b gold/grounded.jsonl
offrig verify calibrate --report-only .offrig/out/calibrate-<model>-<time>
```

`offrig index` 嵌入了 Google DeepMind 的 **EmbeddingGemma 2**（Apache-2.0 许可，无需特殊授权）。
由 [`tools/embed-serve`](tools/embed-serve/) 在 CPU 上提供服务（`embed_url`，默认值）
`http://127.0.0.1:11490`，它既不是共享服务器，端口也不是 11434，也不是 GPU。它取代了
在 1.2.0 版本中，通过对工作室内部查询进行预先注册的基准测试，实现了 nomic-embed-text。
（密集型 nDCG@10 +0.112）；在 `offrig index --rebuild` 之前，基于公理构建的索引将被拒绝。一旦……
该项目有一个索引，`offrig_memory_search` 默认设置为混合模式（关键词加语义）。
`offrig verify calibrate` 将本地环回模型作为验证器进行评分，并针对一个黄金标准案例进行验证。
每次通话时，都需要提供与该声明相关的证据。模型名称中不能包含“cloud”一词。
`--num-ctx` 默认设置为 `auto`，并在程序运行时调整一个窗口的大小。`--keep-thinking`
仅将思考过程记录写入运行目录中的文件 `thinking.jsonl`。`--jobs <N>`
（1 到 8，默认值为 1）允许同时处理最多 N 个待处理的请求，并最终提交这些请求。
文件顺序：服务器必须允许 N 个请求并行处理（`OLLAMA_NUM_PARALLEL`，默认情况下）。
（例如，在 Ollama 中设置为 1）或在其中添加额外的请求队列，所需的 KV 内存会随着并行处理的数量而增加。
插槽和并发性可能会影响结果，直到进行测量时才会确定。
[请参考手册](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/)
列出各个国家的国旗。

### 通道：每个项目一个 sidecar，没有冲突

两个项目可以同时在一个 RunPod 帐户上运行 Side-car。每个项目都获得自己的**通道**：一个 SSH 别名、一个隧道端口和一个 Pod 名称标签，其他项目不共享。

| | 普通通道（CLI、应用程序、Zed） | 项目的通道 |
|---|---|---|
| SSH 别名 | `offrig` | `offrig-<tag>` |
| 隧道端口 | `11435`（运行器 `11436`） | 第一个可用的 `11500`、`11502`、...（运行器：上面的端口） |
| Sidecar 端口（shell 驱动程序） | 无 | `11700` + 通道的插槽：`11700`、`11701`、... |
| Pod 名称 | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| SSH 块 | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` 来自项目文件夹的名称（`aspire-si`、`ai-jam-sessions`），当两个项目共享一个文件夹名称时，会添加一个简短的哈希值。项目的通道会在第一次计划会话时分配，写入 offrig 的配置目录中的 `lanes.toml`，并保留：每次重启后，相同的项目都会获得相同的通道。分配会使用一个锁文件，并以原子方式写入注册表，因此，同时启动的两个 sidecar 不会共享标签、别名或端口。没有通道可以设置为 `11434`（本地 Ollama 的端口）：范围从 `11500` 开始，并且如果编辑注册表以指定其他值，则会拒绝。计划会记录其通道，并且其启动、运行器、看门狗和关闭都会使用该通道，而不是全局配置。

Sidecar 始终只匹配、列出或停止为其自身通道命名的 pod。另一个项目的通道、普通通道的 `offrig-<profile>` pod 以及帐户上的任何其他 pod 都会被忽略：用于检查活动 pod 的启动检查仅检查其自身通道，关闭操作会拒绝名称不是计划通道的 pod，并且隧道的孤立检查仅在它的转发和别名是通道自身时才会删除过时的 `ssh`。在通道存在之前创建的计划不会记录通道，并且会继续在普通通道上运行，因此，在旧方案下启动的 pod 将由启动它的相同计划关闭。

**Sidecar 自身的端口。** `offrig-mcp` 通过 stdio 传输 MCP。一个 shell 驱动程序会为整个会话保留一个端口（当会话自身的 MCP 连接出现问题时），并将其置于一个环回 HTTP 端口之后，并且该端口曾经是整个机器的一个数字（`11439`）：第二个项目的驱动程序或任何其他程序都可以占用它，并且第一个 sidecar 会在没有任何提示的情况下停止工作。现在默认设置为每个项目，来自项目的通道，就像隧道端口一样：通道插槽 `i`（隧道端口 `11500 + 2i`）会获得 sidecar 端口 `11700 + i`。
范围 `11700` 到 `11763` 位于每个通道可以拥有的隧道和运行器端口之上（`11500` 到 `11627`）、普通通道的 `11435` 和 `11436`，以及本地 Ollama 的 `11434`，因此 sidecar 端口绝不能是隧道端口。不会存储任何新内容：`lanes.toml` 未更改，并且端口从通道派生。`OFFRIG_SIDECAR_PORT` 仍然会覆盖它；如果值不是端口、低于 1024，或者为 `11434`、`11435`、`11436` 或通道隧道范围内的任何值，则会拒绝。

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

使用 `--check`，如果某个端口已被占用，则会报告错误，并指明该端口，如果某个 offrig 侧服务程序在该端口上响应，则会显示它所服务的项目：`侧服务程序端口 11700 已被占用：某个 offrig 侧服务程序已在该端口上为项目 <path> 提供服务。请先停止该服务，或者将 OFFRIG_SIDECAR_PORT 设置为未使用的端口`。此检查会验证驱动程序已如何响应（即，请求一个没有任何服务程序提供的项目，驱动程序在访问任何工具之前会拒绝该请求），因此它不会对正在运行的侧服务程序产生任何影响。`offrig_status` 报告通道的 `sidecar_port`。

**一个通道，一个活动 pod。** 一个通道有一个 SSH 别名和一个隧道端口，因此它一次只能服务一个 pod：通道中的第二个 pod（`offrig-<tag>-job` 位于 `offrig-<tag>-jam` 旁边）会将别名重新指向自身，并将第一个计划的 `offrig_put`、`offrig_exec` 和 `offrig_get` 发送到错误的机器。因此，`offrig_launch` 会拒绝，直到通道具有一个打开的计划或任何活动 pod，并显示 `lane <tag> has a live pod <name> (plan <id>); shut it down first`, before anything is committed or rented. The plain lane's `offrig up`，应用程序也会以相同的方式拒绝另一个配置文件的 pod（相同配置文件的 pod 仍然会被重用）。关闭第一个计划，然后启动下一个。

## 层级

配置文件位于 `%APPDATA%\offrig\config.toml`（首次更改时写入）。默认值：

| 配置文件 | GPU | 模型 | 典型成本 |
|---|---|---|---|
| 小型 | 1 × RTX 2000 Ada / A4000 型号 | `qwen3:4b` | 大约 0.25 美元/小时 |
| 中型 | 1 块 RTX PRO 6000（96 GB）；如果没有可用的，则使用 A100 或 H100，均为 80 GB。 | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | 2.09 美元/小时（A100 备用 1.59 美元） |
| 前沿 | 4 块 RTX PRO 6000（总计 384 GB），**SGLang** | Qwen3-Coder-480B AWQ 4 位量化（252 GB），剩余约 130 GB 用于上下文。 | 8.36 美元/小时 |
| 前沿迷你 | 1 块 RTX PRO 6000，**SGLang** | Qwen3-Coder-30B FP8（31 GB）：探索前沿引擎的途径，以低成本进行实践。 | 大约 1.7 美元/小时 |
| 前沿迷你-awq | 1 块 RTX PRO 6000，**SGLang** | Qwen3-Coder-30B AWQ（17 GB）：一种经济高效的、用于前沿模型的 4 位 MoE 内核。 | 大约 1.7 美元/小时 |
| 作业 | 1 块 RTX PRO 6000（96 GB）；如果没有可用的，则使用 A100 或 H100，均为 80 GB。 | 无：一个 **作业 pod** 运行您的工作，而不是模型服务器 | 2.09 美元/小时（A100 备用 1.59 美元） |
| 阻塞 | 首先使用 1 块 A40（48 GB）的存储设备；如果没有可用的，则使用 A6000、A5000、3090 或 L4，或者使用 4090。 | 无：一个用于 ai-jam-sessions 项目中歌曲渲染的“任务容器”（SoulX-Singer）。 | 0.49 美元/小时（A40） |

一个使用`recipe`的配置会运行与Ollama不同的引擎：一个固定的镜像（`lmsysorg/sglang:v0.5.20-cu130`），一个在启动时下载的Hugging Face模型，以及额外的服务器参数。offrig会根据GPU的数量设置张量并行性，根据配置设置上下文长度，并将引擎保持在pod的环回地址上；一个配方不能覆盖这些设置。对于一个受限的仓库，`hf_token_secret`指定一个RunPod类型的密钥，并以`{{ RUNPOD_SECRET_<name> }}`的形式引用，这样令牌就不会进入pod的配置中。启动时，会报告磁盘上的权重，同时引擎会下载这些权重，如果引擎退出，则会立即停止（并显示引擎的日志）。当`/health`返回结果时，它不会将引擎标记为已就绪，并且`/v1/models`的响应也不是就绪的标志。在`/health`返回200之后，offrig会发送一个聊天完成请求（`max_tokens`为1，`temperature`为0），并等待最多30秒。只有当该回复包含一个选项时，才会执行下一步（`ready`）。如果完成请求失败或超时，offrig会在15秒后再次尝试。如果第二次尝试也失败，则执行下一步（`engine_unresponsive`）：作业将继续运行，错误和引擎日志将保留在作业中。下一步的动作是“引擎没有响应完成请求：关闭pod，或者如果它仍在预热中，则等待”。offrig不会为此关闭pod，无论是侧边栏、CLI还是应用程序。缓慢的第一个请求（图形捕获、JIT）不是丢弃下载和加载的理由；计划的截止日期仍然限制了成本。挂起计时器从同一ssh读取的`Application startup complete`的`grep -c`开始，读取整个引擎日志，而不仅仅是最后40行。一个活跃的引擎可能会将启动行推到日志的末尾。计数仍然会启动计时器。如果该计数至少为1，并且`/health`在接下来的3分钟内保持不变，则执行下一步（`engine_hung`）：显示最后40行、自首次看到计数以来的秒数，以及下一步的动作（`shut the pod down: the engine finished startup but never became healthy`）。
offrig会报告挂起情况，并保持pod运行。关闭pod的决定由您自行决定，与GPU数量不匹配的情况相同。GPU数量警告不会附加到下一步的动作或未响应的动作中，因此句子保持不变。已经退出的引擎进程也不会报告。在侧边栏中，补偿器会删除该pod。在CLI和应用程序中，命令会停止并显示错误，但不会删除它。

每个配置都按优先级顺序列出 GPU 类型。v2 只选择一种类型，并且不会回退，因此 offrig 会按顺序尝试该列表，并且永远不会超过计划的价格。如果没有可用的 GPU，配置可以等待（`wait_for_gpu_minutes`；frontier 最多等待 120 分钟）：offrig 每分钟检查一次，并在 GPU 变为可用时创建 pod。在等待期间，不会租用任何资源，按 Ctrl+C 或应用程序的“取消启动”按钮停止，如果实时目录不可用，它会简单地每分钟重新尝试创建。大型多 GPU 设置可以在几分钟内完成。价格是 v2 目录中的安全云列表价格（`GET /catalog/gpus`），每个 GPU 乘以 GPU 数量。如果列表中显示“无”，则表示没有可用的 GPU。公共定价页面不是可用的价格。`CUDA_VERSIONS` 仅在 GPU 的 `cudaVersions` 列表中缺少时才用作回退。

### 等待容量

使用 `no_fallback` 或 `max_price_hr` 缩小范围的计划经常会遇到容量不足的问题，因此启动会安静地重试，而不是失败，并在等待期间不租用任何资源。等待时间为：计划的 `wait_minutes`（一个 `offrig_plan` 参数，与计划一起存储；`0` 立即失败），否则为配置文件的 `wait_for_gpu_minutes`。`job` 配置文件的默认值为 20 分钟。等待时间会缩短到计划剩余时间减去五分钟的储备时间，因此它永远不会超过计划的截止日期，并且由于在等待期间不租用任何资源，因此不会增加承诺的最坏情况。`offrig_launch` 报告 `capacity_wait_minutes`；在等待期间，`offrig_job` 显示 `progress.capacity_wait`（`checks`、`waited_secs`、`limit_secs`）和一个步骤，说明这是哪个检查。当等待时间结束时，启动将失败，并显示 `no capacity`，并且没有租用任何资源。

### 固定计划的硬件

配置按优先级顺序列出 GPU 类型，并且 RunPod 选择具有可用资源的第一个 GPU，因此，如果没有限制，计划可能会选择一个具有较少内存、较旧驱动程序和不同价格的回退卡。三个限制可确保计划使用其工作可以使用的硬件。规划仍然是免费的；这些限制只是缩小了计划可以租用的范围。

| 限制 | 位置 | 效果 |
|---|---|---|
| `min_cuda` | 配置 (`config.toml`) | 最旧的主机 CUDA（驱动程序）版本，来自 RunPod 的列表（`13.2`、`13.1`、`13.0`、`12.9`、... `11.8`）。pod 创建会将该值作为 `gpu.minCudaVersion` 发送，因此，即使主机比复制的版本更新，仍然符合条件。当目录为某个 GPU 返回 `cudaVersions` 时，计划仅保留该类型，如果该类型具有可用版本，并且该版本等于或高于此值。当目录省略该列表时，复制的列表将用作回退。对于作业配置，使用此值和映像自身的 `[profiles.job] min_cuda` 中的较大值。 |
| `min_vram_gb` | 配置 | 计划可以接受的最小总 VRAM（配置的所有 GPU 的总和）。低于此值的方案将被丢弃；如果某个类型 RunPod 列出的内存为零，该类型也将被丢弃。 |
| `max_price_hr` | `offrig_plan` 参数 | 该 Pod 的总成本最高为多少，即所有 GPU 的总小时费用（如图 `offrig_offers` 所示）。高于此数值的报价将被拒绝，并且现在没有列出价格的类型也将被拒绝（无法保证其价格上限）。 |
| `no_fallback` | `offrig_plan` 参数 | 仅允许配置的第一个 GPU 系列。两个 RTX PRO 6000 Blackwell 版本（服务器版和工作站版）属于一个系列；其他所有卡，包括 A100 SXM 和 PCIe，都属于各自的系列。 |

配置字段是可选的，并且默认情况下为空，因此由早期 offrig 编写的 `config.toml` 不会发生更改。 `job` 配置设置 `min_cuda = "13.0"`。

`cuda_versions` 是 `min_cuda` 之外的一个可选的精确列表，例如 `cuda_versions = ["13.0"]`。如果设置了该值，pod 创建会将该列表作为 `gpu.allowedCudaVersions` 发送，按写入的顺序发送，并且不会同时发送最小值。`offrig_plan` 报告该列表。同时设置它和 `min_cuda`，包括作业映像自身的最小值，是不允许的：两者结合使用会产生歧义。每个版本都必须是 offrig 可以接受的版本。该列表按从新到旧的顺序排列，为 13.2、13.1、13.0，然后是 12.9 到 11.8。13.2 和 13.1 比 RunPod 记录的 v1 枚举（于 2026-10-11 阅读，该枚举停止于 13.0）更先进。它们被列出，因为在 2026-10-11，一个写入 pod 被放置在一个 13.2 主机上。最小值仍然只发送 `gpu.minCudaVersion`。

`offrig_plan` 根据剩余资源对最坏情况进行定价：`(max_hours + 780 s) x min(max_price_hr, the dearest listed price among the remaining GPUs)`（四舍五入到美分；780 秒是 pod 上的 120 秒自停止余量加上 660 秒重试时间）。如果没有 `max_price_hr`，那么就是配置中列出的最昂贵的价格，如前所述。如果计划中没有剩余资源，则会拒绝该计划，并说明每个被丢弃的 GPU 的原因，并且不会写入任何内容。

该计划存储剩余的 GPU 列表，以及 CUDA 的最低值或确切的 CUDA 列表，并且 `offrig_launch` 仅从计划中存储的内容进行租用，而不会从配置的后续编辑中进行租用。 `offrig_job` 和侧边栏的启动结果报告 GPU 类型和主机实际租用的 CUDA 版本，以 `rented` 对象的形式呈现。Pod API 不报告主机的 CUDA 版本，因此一旦 SSH 启动，启动程序将通过通道运行 `nvidia-smi`，并从标头中读取该版本（`CUDA Version: 12.8`，或较新驱动程序上的 `CUDA UMD Version: 13.4`）；`rented.cuda_source` 显示 `nvidia-smi` 或 `pod API`。如果主机的 CUDA 版本低于计划的最低值，或者 GPU 不在计划中列出，或者价格高于计划，则 `offrig_job` 返回一个 `warnings` 条目，并以 `WARNING` 启动 `next_action`。没有任何内容会自动终止：停止租用由调用者决定（`offrig_shutdown`）。如果 Pod API 或 `nvidia-smi` 都没有提供 CUDA 版本，则 `rented.notes` 会说明这一点，并且不会检查最低值。

### 容器磁盘

一个 Pod 有两个磁盘：容器磁盘（位于主机本地），以及挂载在 `/workspace` 处的卷。在某些主机上，`/workspace` 是一个缓慢的网络文件系统：一个作业 Pod 在那里测得的速度为 32 MB/s，而在其容器磁盘上为 354 MB/s，并且无法及时提取大约 130 GB 的模型，而容器磁盘只有 60 GB。容器磁盘的大小是配置中的 `container_disk_gb`（内置配置中为 30 到 60 GB；`job` 配置为 60 GB），并在 Pod 创建时作为 `disk` 传递。 `offrig_plan` 接受 `container_disk_gb`（1 到 2000）以覆盖该值，用于一个计划；该计划存储该值，启动程序发送该值，并且计划的回复和 `offrig_status` 显示强制执行的大小。

- 容器磁盘**不收取费用**：offrig 仅收取 GPU 时间费用，因此该方案的最坏情况与任何大小的方案相同。RunPod 会收取磁盘费用；它报告的 pod（`offrig_job` 显示）的价格是否包含容器磁盘，此处尚未进行检查。
- offrig 不会自动移动您的下载文件。作业命令以 `HF_HOME` 开头，在 `/workspace` 卷（`/workspace/hf`）上运行；要使用容器磁盘，请在命令中设置您自己的 `HF_HOME=/root/hf python ...`。
- 容器磁盘会在 pod 停止时被删除，就像没有网络卷的卷一样：在 `offrig_shutdown` 之前，使用 `offrig_get` 将结果复制回去。
- 1 到 2000 的限制是 offrig 自身的错误检查，以防止输入错误；RunPod 的实际限制未进行检查。

### 作业 Pod

一个包含 `job` 的配置，用于租用 GPU，以便运行在其上运行的工作，例如训练运行，而不是用于提供模型。它的 pod 运行一个固定的 PyTorch 镜像（`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`，Blackwell 的 CUDA 12.8），其中包含 sshd 且没有其他内容：

- 它不提供任何模型，因此没有隧道，也没有任何内容与 Zed 连接。sshd 根本不允许任何端口转发（`AllowTcpForwarding=no`）；唯一的访问方式是通过 ssh 连接到 pod。
- 作业配置不列出任何模型，并且也不能同时具有配方；配置检查会拒绝两者。
- `offrig up` 和应用程序在租用任何内容之前都会拒绝作业配置。它通过以下方式运行：侧边栏：`offrig_plan profile=job`、`offrig_launch`，然后是 `offrig_put`、`offrig_exec` 和 `offrig_get`。当 sshd 响应时，启动就准备好了。
- 命令在 pod（`setsid nohup`）上的 `/workspace/job` 中以分离模式运行，因此它会持续存在于侧边栏和 ssh 会话之后。它以 base64 的形式发送，因此 ssh shell 不会读取其中的任何内容。它的日志和退出状态保存在 `/workspace/offrig/jobs/` 中。Hugging Face 下载会转到 pod 卷上的 `/workspace/hf`。
- `offrig_exec action=run` 用于快速检查（`ls`、`nvidia-smi`），而不是用于实际工作：它在 `timeout` 下运行命令，直到完成（默认 30 秒，最多 120 秒），并返回 `stdout`、`stderr`、`exit_code` 和 `timed_out`。输出将被截断为每个流的最后 64 KB（`truncated`），并且是不受信任的 pod 输出。需要更长时间的命令是一个 `start`。
- 作业日志的末尾会折叠进度条：tqdm 样式的重绘通过回车符连接，仅显示其最后一帧。`offrig_exec action=status save_log=<local path>` 还会将作业的完整日志（按写入方式）复制到本地文件（创建父文件夹），因此日志的末尾可以保持较短。
- 该镜像是一个 CUDA 12.8 构建，因此作业配置会命名它运行的旧est 主机 CUDA 版本（`min_cuda = "12.8"`），并且 pod 将使用该版本作为 `gpu.minCudaVersion` 创建。如果没有这样做，则具有较旧驱动程序的主机会启动 pod，并且在租用开始后，torch 将找不到 GPU。实际工作可能需要比镜像更多的内容：`job` 配置还会在配置上设置 `min_cuda = "13.0"`（参见上方），因为其运行的作业会安装当前的 vLLM，其 PyTorch 是 CUDA 13 构建。
- 预算、计划、看门狗和关闭操作与任何其他配置一样。在 `offrig_shutdown` 之前，将结果复制回去：pod 的磁盘会随其一起删除。
- `jam` 是 ai-jam-sessions 渲染其歌唱的作业配置：SoulX-Singer 比训练卡需要的要少得多，因此它租用了一个廉价的 24-48 GB 卡。设置和会话都位于该仓库（`docs/vocal-offrig.md`）中；offrig 不了解任何关于歌唱的内容。

### 在网络卷上暂存权重

配方配置会在每次启动时下载其权重：对于 frontier，准备时间约为 20/22 分钟（252 GB，8.36 美元/小时）。置入会将它们一次性置于 RunPod 网络卷上：

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- 无论 pod 是否运行，该卷都会按月计费（frontier 的 300 GB 约为 21 美元/月，价格为 0.07 美元/GB），因此只有人类才会进行置入；没有代理工具可以进行置入。
- 卷位于一个数据中心，因此配置的 pod 随后仅在该数据中心启动，并且提供和计划的价格是在那里定价的。选择一个具有网络存储和配置的 GPU 的数据中心；`offrig gpus` 和 RunPod 的控制台显示它们的位置。
- 下载在可用在该数据中心中最便宜的 GPU pod 上运行。在成功、失败或超时时，pod 将被终止。
- 在下载开始之前，卷会被记录在配置中，因此失败的置入永远不会被忘记；重新运行以恢复，或 `--remove`。
- 置入的启动会离线运行 Hugging Face，仅在置入完成（卷上的标记）时才会运行。半置入的卷会下载剩余部分，而不是失败。

## 资金安全

- 在启动之前，offrig 会显示最便宜的可用匹配项以及 pod 运行时您的剩余可用时间。如果剩余可用时间少于一小时，除非您覆盖，否则它会拒绝，因为在达到零时，RunPod 会停止帐户中的每个 pod，包括 offrig 不管理的 pod。
- 自动停止会在每个 GPU 负载低于 5% 后 30 分钟后终止 pod（可配置，或关闭）。
- 在 pod 运行时关闭应用程序时，会询问是否终止它或使其继续运行。
- offrig 仅触及它命名的 pod：`offrig-<profile>` 用于 CLI 和应用程序，`offrig-<tag>-<profile>` 用于项目的侧边栏通道（参见通道）。侧边栏绝不会触及另一个通道的 pod、普通通道的 pod或任何其他 pod；这些 pod 会被列出，但不会被更改。
- 对于代理会话，在进行任何支出之前，会强制执行上限：计划的最坏情况（实时价格 ×（最大小时数 + 780 秒自动停止尾部），向上取整到美分）会与人类设置的预算进行比较，并且如果超过该预算，则会拒绝，并且启动仅需要一个计划 ID，因此代理无法自行定价。
- 每个侧边栏启动都有一个看门狗，该看门狗会在计划的截止日期时终止 pod，并且运行程序会在队列为空时关闭 pod。

## 它会更改您机器上的哪些内容

| 什么 | 位置 | 撤销 |
|---|---|---|
| Zed 提供程序 `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`；第一个原始版本将保留为 `settings.json.offrig.bak` |
| Zed 默认模型（仅在您要求时） | 相同的文件 | `offrig zed-remove` 恢复之前的默认设置 |
| `OFFRIG_API_KEY`（占位符；Zed 需要一个密钥） | 用户环境 | `setx OFFRIG_API_KEY ""` 或在系统属性中删除它 |
| SSH 别名 `offrig` | `~/.ssh/config`，位于 `# >>> offrig:offrig >>>` 标记之间 | 删除标记的块 |
| SSH 别名 `offrig-<tag>`，每个从侧边栏启动的项目一个 | `~/.ssh/config`，位于 `# >>> offrig:offrig-<tag> >>>` 标记之间 | 删除标记的块 |
| 项目通道 | `%APPDATA%\offrig\lanes.toml`（项目路径、标签、别名、隧道端口） | 在通道中没有 Pod 运行时，删除项目的条目，或者删除整个文件 |
| Pod 主机密钥 | `~/.ssh/known_hosts_offrig` | 删除该文件 |
| 设置 | `%APPDATA%\offrig\config.toml` | 删除该文件 |
| 暂存的权重（仅限 `offrig stage --yes`） | 一个 RunPod 网络卷 `offrig-<profile>`；按月计费 | `offrig stage <profile> --remove --yes` |

Zed 设置中的注释和布局将被保留：编辑将通过 JSONC 语法树进行。

## 威胁模型

- **RunPod API 密钥。** 从 `RUNPOD_API_KEY` 读取；绝不会写入磁盘或日志。Zed
提供程序故意不命名为 `runpod`，因为这样一来，Zed 就会读取
将 `RUNPOD_API_KEY` 发送到模型服务器。
- **模型服务器。** 只能通过 SSH 密钥访问。已禁用密码登录。
容器内部，以及 sshd 仅允许本地端口转发。
- **主机密钥。** 每个终端都单独存储在独立的“known_hosts”文件中。offrig 忘记了某个密钥。
只有当 Pod 的端点发生变化时，才会触发，因为 RunPod 在不同的 Pod 之间重复使用 IP 地址和端口组合。
- **Shell 注入。** 在使用之前，会对模型名称进行验证，以确保其符合 Ollama 的命名规则。
他们到达了一个偏远的贝壳滩。
- **孤立的通道。** 如果 offrig 死亡，其 `ssh` 可能会继续占用端口。在下一次
一开始，Offrig 就能将其击败，但前提是听者必须携带 Offrig 的确切物品 `ssh.exe`。
仅允许使用指定的正向规格。端口上出现的任何其他内容都会被拒绝，并且不会被终止。
- **不收集遥测数据。** offrig 只与 RunPod 的 API、您的 Pod 以及本地 Ollama 进行通信。
（用于比较模型列表）。

## 测试

`cargo test --workspace` 运行 250 多个测试，涵盖至少 90% 的代码行（低于此值 CI 将失败）：

- **核心库：** RunPod 解析、两种引擎的 pod 规范、SSH 配置、Zed JSONC 编辑、防护规则、成本和空闲逻辑、存储及其迁移、角色、上下文组装、确定性检查、运行器的决策、看门狗以及分阶段加载，包括一个模拟 RunPod，证明失败的分阶段加载会终止其 pod。
- **应用程序：** 状态处理以及 egui 测试框架中的点击式 UI 测试。
- **CLI：** 退出代码、日志级别，以及 API 密钥绝不会出现在输出中。
- **辅助进程：** 通过 stdio 与模拟 RunPod 进行端到端测试，以及与真实的看门狗进程和真实的运行器进程（针对模拟的 pod 模型）进行测试；每个工具错误都会带有一个代码。

`scripts/verify.sh`（或 `scripts/verify.ps1`）运行格式检查、clippy、测试以及每个二进制文件的模拟运行，所有这些都通过一个命令完成。CI 还运行 `cargo deny`，对 `Cargo.lock` 进行 OSV 扫描，并将结果发送到 Codecov 和 `atlas check`。

### 实时测试记录（2026-10-02，中等级别，A100 80GB，约 0.45 美元）

- Pod 大约在 80 秒内启动；sshd、隧道和 Pod 的 Ollama 0.35.0 均已响应。
- Pod 上提取了 97 GB 的模型，速度约为 150–250 MB/s。
- `qwen3-coder:30b-a3b-q8_0` 和 `gpt-oss:120b` 都通过隧道回答了一个流式聊天，并进行了正确的工具调用。它们使用了 Pod 的 36 GB 和 64 GB 的 VRAM；本地 Ollama 没有加载任何内容，并且根本不在本地 GPU 上。
- 所有七个保护检查均已通过，来自 CLI 和应用程序。
- 突然终止的 CLI 导致其 `ssh` 保持端口打开；下一次运行会重新获取该端口。
- 应用程序的隧道、检查、模型测试和关闭均通过其按钮进行驱动。

实时运行发现并已修复的错误，现在已覆盖：一个后台 `&&` 列表会使 ssh 的 stdout 保持打开状态并导致提取开始时出现问题；Pod 列表缺少没有 `includeMachine=true` 的 GPU 类型；启动检查会重复计算正在运行的 Pod 的价格。

### 侧边栏预演（2026-10-03，小型层级，RTX 2000 Ada，已预订 0.08 美元）

通过 stdio 驱动的已安装 `offrig-mcp`，就像代理调用它一样：

- `offrig_plan` 估算 0.5 小时的价格为 0.15 美元（最坏情况）；`offrig_launch` 提交了它，启动了看门狗，第二次调用返回了相同的工作。租用了一个 pod，价格为 0.24 美元/小时。
- 租用后 100 秒，SSH 启动；`qwen3:4b` 拉取，并在 150 秒时准备就绪。
- `offrig_ask` 运行了一个游戏设计师交接任务，耗时 46 秒；回复满足了其验收检查，并保留了内存中的五条约束。
- pod 访问了互联网（维基百科、GitHub API）。根据 pod 提取的数据，模型正确回答了当前的问题；如果未提供任何数据，它会说它没有实时访问权限。
- `offrig_shutdown` 来自新的辅助进程终止了 pod 并关闭了账目；看门狗检测到计划已关闭并退出。本地 GPU 在整个过程中都处于空闲状态。

发现并已修复：Pod 一次处理一个请求（`OLLAMA_NUM_PARALLEL=1`）；四个插槽从 40 到 102 tok/s 在同一 GPU 上处理了 8 个并行请求，因此现在每个配置都有 `parallel = 4`。 `complete` 在没有给出任何理由的情况下被拒绝（现在默认设置为“验收检查已通过”；失败仍然需要一个理由）。状态建议在会话进行时记录一个简短的片段。正在考虑将泄漏到回复中的文本删除，并且如果回复因正在思考而为空，则会提示提高 `max_tokens`。

### 运行器预演（2026-10-03，小型层级，RTX 2000 Ada，已预订 0.04 美元）

五个交接，其中一个依赖于另一个，由 `offrig_run` 完成，没有人进行驱动：

- 四个交接同时在四个插槽上进行（16 GB VRAM 中的 12.9 GB）；依赖项在依赖项完成后立即启动，并基于其结果进行构建。
- 三个检查涵盖验收的交接自行完成；竞争对手的背景故事（部分检查）和故事（没有检查）被发送到审查。
- 审查将故事发回（“这条河流以项目命名”）；实时运行器采用它并根据反馈进行修改（“Veyl 河”）。
- 队列在 6.5 分钟内完成（6 轮，20,861 个令牌）；运行器自行关闭了 Pod。

已学习：确定性检查验证结构，而不是设计质量。4B 模型通过了“三个动词”，但动词比较简单，因此 `accept_on_checks` 用于结构性工作，而设计工作则交给审核。qwen3:4b 在每一轮中花费了大约 4,000 个令牌进行思考，即使只有三行故事。一个队列仅在其中包含足够的独立交接任务时，才会使每个插槽保持繁忙；依赖链一次执行一个。

### Frontier 和 SGLang 运行（2026-10-03，预订 4.27 美元）

| 运行 | Pod | 准备就绪后 | 队列 | 已预订 |
|---|---|---|---|---|
| 前沿迷你版（Qwen3-Coder-30B FP8） | 1 × RTX PRO 6000 | 5.5 分钟 | 15 秒内完成 4 次数据传输 | $0.30 |
| 前沿迷你 AWQ 模型 (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 分钟 | 4 次数据传输 | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 分钟**（以 278 MB/s 的速度加载 252 GB 数据，然后开始） | **20 秒内完成 31 次数据传输** | **$3.59** |
| 30B 规模的集群扫描 | 1 × RTX PRO 6000 | 10 分钟（缓慢的 Pod 部署） | 仅进行扫描 | $0.43 |

- SGLang v0.5.20 (cu130) 在 Blackwell 上运行：flashinfer 注意力机制，`awq_marlin` 用于 4 位 MoE 权重，通过 PCIe 在四个卡上进行张量并行；`/dev/shm` 是 352 GB。
- frontier 的表现明显优于小型模型：在场景中的对话，以及一个编译并通过了三个测试（本地检查）的 Rust 模块。30B FP8 运行的相同任务的版本无法编译。
- 一个已加载的模型服务于整个蜂群；不需要复制。并发扫描，每个回复包含 384 个令牌，每秒总令牌数：

| 代理 | 480B，使用 4 个 GPU | 30B AWQ，使用 1 个 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

随着代理数量的增加，每个代理的速度下降（480B：88 → 64 时为 24 个令牌/秒），但总吞吐量持续上升；480B 的增益在超过 256 后趋于平稳。frontier 的 KV 缓存包含 398,526 个令牌，因此，在实际切换上下文为 2-8k 令牌的情况下，frontier 级别的并发数为 64，而单卡 SGLang 级别的并发数为 32。

在过程中发现并修复了以下问题：修订后的输出填充了额外的字符，以通过标题检查（现在是一个内置的 `no_repeats` 检查，并且修订后的内容会进行原地重构）；某个角色的必需输出意外地泄漏到最终交付物中（现在，数据传输会设置格式）；标题反馈会命名 Markdown 格式；未更改的修订版本会停止，而不是重复。

尚未在实际环境中验证：从 Zed 的代理面板发送的聊天（直接测试 Zed 使用的请求形状），以及 1.1.0 准备检查、挂起报告和在租用的 Pod 上的精确 CUDA 引脚（请参阅 [状态](#状态)）。

## 标准合规性

根据工作室的工作流程标准进行评分（0 表示缺失，1 表示部分符合，2 表示符合，3 表示优秀）。

- **PIN_PER_STEP：2。** Pod 镜像固定到版本标签（`ollama/ollama:0.35.0`、`lmsysorg/sglang:v0.5.20-cu130`；一个配方拒绝使用 `latest`），编译器固定到 1.98.1，依赖项通过 `Cargo.lock` 固定，Atlas 引擎固定到集群的 1.24.0。每个切换都记录其模型、角色哈希和提示哈希。模型通过标签或仓库 ID 固定，而不是通过摘要固定。
- **ANDON_AUTHORITY：3。** 每个步骤都会在出现缺陷时停止运行：权重超过磁盘大小的计划在任何支出之前都会被拒绝；失败的拉取会停止启动；Zed 编辑无法读回，因此不会写入；损坏的设置文件会被报告，但不会被重写；CI 会阻止 fmt、clippy、测试、许可证和建议。
- **NAMED_COMPENSATORS：2。** 每个不可逆操作都有一个撤销操作，如下所示。
- **DECOMPOSE_BY_SECRETS：2。** 每个更改自身原因的模块只有一个：RunPod 的 API（`runpod`）、Pod 的内容（`spec`）、传输（`tunnel`、`remote`）、每个本地文件 offrig 编辑（`sshconfig`、`zed`）以及规则（`guard`、`cost`）。前端不包含超出呈现之外的任何逻辑。
- **UNCERTAINTY_GATED_HUMANS：2。** offrig 仅在结果代价高昂或有损时才询问：在剩余运行时间不足一小时的情况下启动，终止 Pod（并说明损失的内容），以及在 Pod 仍在计费时退出。有两个决定完全由人类做出，并且没有代理工具可以做出这些决定：预算上限和分阶段部署卷，后者按月计费。无法通过代码检查的切换输出将在审核中等待，而不是完成。
- **EXTERNAL_VERIFIER：n/a。** 没有特殊声明。

**补偿器**

| 操作 | 撤销 | 撤销后的状态 | 所有者 |
|---|---|---|---|
| 创建一个 Pod（开始计费） | `offrig down <profile> --yes`，应用程序关闭，或自动停止 | Pod 已终止，停止计费 | 运行 offrig 的操作员 |
| 终止一个 Pod | 其磁盘上没有内容；重新启动配置文件，模型会重新下载（网络卷会保存这些模型） | 新的 Pod，相同的配置文件 | 操作员 |
| 写入 Zed 提供程序或默认模型 | `offrig zed-remove`，或恢复 `settings.json.offrig.bak` | 与之前一样，使用 Zed 和 offrig | 操作员 |
| 设置 `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""`，或在“系统属性”中删除它 | 变量已删除 | 操作员 |
| 写入 SSH 别名 | 删除 `~/.ssh/config` 中标记的块 | 配置与之前相同 | 操作员 |
| 分配一个项目通道（项目的第一个 `offrig_plan`） | 一旦其通道中没有运行的 Pod，就从 `lanes.toml` 中删除该项目条目；新的计划会再次分配 | 通道可供重新使用；别名块是单独的（在上一行） | 操作员 |
| 写入通道的 SSH 别名块（一个辅助启动） | `offrig_shutdown` 在命名计划的 Pod 时会删除它（也包括在启动失败后）；否则，删除 `~/.ssh/config` 中的 `# >>> offrig:offrig-<tag> >>>` 块 | 配置与之前相同；其他通道的块不受影响 | 操作员 |
| 在 Pod 上拉取一个模型 | `ollama rm <model>` 在 Pod 上，或者终止 Pod | 模型已删除 | 操作员 |
| 终止一个孤立的 offrig 隧道 | 不需要；只有具有该通道的确切别名和转发的 `ssh` 才会终止，绝不会终止其他通道的 | 端口已释放 | offrig |
| 辅助启动（`offrig_launch`） | `offrig_shutdown`；如果设置失败，则自动启动；在截止日期时，看门狗会启动；当其队列为空时，运行程序会启动 | Pod 已终止，计划已关闭，并计算了实际花费 | 调用代理，看门狗作为备用 |
| 准备一个卷（`offrig stage --yes`，每月计费） | `offrig stage <profile> --remove --yes` | 卷已删除，配置文件已恢复到下载状态 | 准备卷的人 |
| 在作业 Pod 上启动一个作业（`offrig_exec action=start`） | `offrig_exec action=stop`，或 `offrig_shutdown` | 作业已终止，包括其启动的所有内容；其日志会保留，直到 Pod 消失 | 调用代理 |
| 在作业 Pod 上运行一个简短的命令（`offrig_exec action=run`） | 仅执行命令本身的操作；最多在 120 秒内由 `timeout` 结束，或者由 `offrig_shutdown` 结束 | Pod 处于命令执行后的状态 | 调用代理 |
| 将文件复制到作业 Pod 或从作业 Pod 复制文件（`offrig_put`，`offrig_get`） | 删除副本（在 Pod 上，`offrig_exec`；此处，文件） | 与复制之前相同 | 调用代理 |

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
tools/embed-serve    the CPU embed server for the project index (Python; EmbeddingGemma 2)
docs/                the side-car's design and its research grounding
atlas/               Atlas map of the repo (regenerate with `atlas map`)
```

## 许可证

MIT。请参阅 [LICENSE](LICENSE)。

---

由 <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a> 构建。
