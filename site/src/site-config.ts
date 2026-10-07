import type { SiteConfig } from '@mcptoolshop/site-theme';

export const config: SiteConfig = {
  title: 'offrig',
  description: 'Run big models on rented RunPod GPUs, never your own. A Windows desktop app, a CLI and an MCP side-car for agents: priced plans under a human-set budget, project lanes, job pods, and a watchdog that ends every pod at its deadline.',
  logoBadge: 'OR',
  brandName: 'offrig',
  repoUrl: 'https://github.com/mcp-tool-shop-org/offrig',
  footerText: 'MIT Licensed — built by <a href="https://github.com/mcp-tool-shop-org" style="color:var(--color-muted);text-decoration:underline">mcp-tool-shop-org</a>',

  hero: {
    badge: 'Windows · RunPod · MIT',
    headline: 'Big models on rented GPUs,',
    headlineAccent: 'never on yours.',
    description: 'offrig rents RunPod GPUs for models too big for your machine, keeps them reachable only through an SSH tunnel, and checks every time that nothing lands on your own GPU. Agents get an MCP side-car that prices each session before it spends, and a watchdog that ends the pod at its deadline.',
    primaryCta: { href: 'https://github.com/mcp-tool-shop-org/offrig/releases', label: 'Download for Windows' },
    secondaryCta: { href: 'handbook/', label: 'Read the Handbook' },
    previews: [
      {
        label: 'Status',
        code: '> offrig status\nbalance $40.00, spending $0.00/hr, runway unlimited\n\n> offrig gpus --count 1\nGPU (x1)                                  VRAM   $/hr   stock\nNVIDIA RTX PRO 6000 Blackwell Server ...  96GB   2.09   Low\nNVIDIA A100-SXM4-80GB                     80GB   1.59   High\n\n(example figures)',
      },
      {
        label: 'Launch',
        code: '> offrig up medium\n  - creating offrig-medium on 1x RTX PRO 6000\n  - ssh is up\n  - qwen3-coder:30b-a3b-q8_0: pulled\n  - gpt-oss:120b: pulled\n  - all profile models are on the pod\n\n# then the seven guard checks run, and the\n# tunnel is held on 127.0.0.1:11435',
      },
      {
        label: 'For agents',
        code: '# An agent plans, then launches only by plan id\noffrig_plan  profile=job max_hours=3.5\n             no_fallback=true max_price_hr=2.2\n  -> worst case $7.32, committed against the cap\noffrig_launch   plan_id=<id>\noffrig_shutdown plan_id=<id>',
      },
    ],
  },

  sections: [
    {
      kind: 'features',
      id: 'features',
      title: 'The guarantee, and how it holds',
      subtitle: 'Seven checks verify it on every launch, from facts offrig can observe.',
      features: [
        {
          title: 'Reachable only through the tunnel',
          desc: 'The model server binds to the pod\'s own loopback and the pod exposes only port 22. offrig\'s tunnel listens on 127.0.0.1:11435 and refuses 11434, your local Ollama\'s port, so a dead tunnel fails instead of falling through to your own GPU.',
        },
        {
          title: 'Weights never land here',
          desc: 'Models are pulled on the pod, by the pod, or read from a staged network volume. A guard check fails if any pod model also exists in your local Ollama.',
        },
        {
          title: 'Zed never switches providers',
          desc: 'The pod\'s models are their own provider in Zed. If the pod is down, choosing one of them errors; Zed does not quietly try a local model.',
        },
      ],
    },
    {
      kind: 'features',
      id: 'agents',
      title: 'A side-car agents can be trusted with money',
      subtitle: 'offrig-mcp is an MCP server. The budget cap is set by a human; an agent cannot name its own price.',
      features: [
        {
          title: 'Priced before it spends',
          desc: 'A plan commits its worst case (live price × max hours) against the cap before anything is rented, and a launch takes only a plan id. Plans can pin the GPU family, a top hourly price and a host CUDA floor.',
        },
        {
          title: 'A watchdog on every pod',
          desc: 'A separate process ends the pod at the plan\'s deadline even if the agent, the session and the side-car are gone. A failed setup terminates the pod instead of leaving it billing.',
        },
        {
          title: 'Lanes keep projects apart',
          desc: 'Each project gets its own SSH alias, tunnel port, side-car port and pod-name tag. One lane holds one live pod, and a side-car only ever touches pods named for its own lane.',
        },
        {
          title: 'Job pods for real work',
          desc: 'A job pod runs your training or rendering instead of a model server: copy files in, start a detached command, follow its log, copy results back, shut down.',
        },
        {
          title: 'Handoff queues that run unattended',
          desc: 'Queue role-headed handoffs with acceptance checks. A detached runner keeps every model slot busy, revises only against failed checks, and shuts the pod down when the queue is empty.',
        },
        {
          title: 'Memory that outlives the pod',
          desc: 'Each project keeps a database of briefs, decisions and outcomes, so a session survives compaction or a restart without re-explaining anything.',
        },
      ],
    },
    {
      kind: 'data-table',
      id: 'tiers',
      title: 'Tiers',
      subtitle: 'Default profiles. Prices are read live from RunPod\'s secure cloud; these are typical.',
      columns: ['Profile', 'GPUs', 'Runs', 'Typical cost'],
      rows: [
        ['small', '1 × RTX 2000 Ada / A4000 class', 'qwen3:4b on Ollama', 'about $0.25/hr'],
        ['medium', '1 × RTX PRO 6000 (96 GB)', 'qwen3-coder:30b and gpt-oss:120b on Ollama', '$2.09/hr'],
        ['frontier', '4 × RTX PRO 6000 (384 GB)', 'Qwen3-Coder-480B AWQ on SGLang', '$8.36/hr'],
        ['job', '1 × RTX PRO 6000 (96 GB) on a CUDA 13 host', 'your own work, no model server', '$2.09/hr'],
        ['jam', '1 × A40 (48 GB) first', 'singing renders for ai-jam-sessions', '$0.49/hr'],
      ],
    },
    {
      kind: 'code-cards',
      id: 'quickstart',
      title: 'Quick start',
      cards: [
        {
          title: 'Install',
          code: '# Windows, with OpenSSH (built in) and a RunPod account\n# 1. Download offrig-<version>-windows-x64.zip from Releases\n#    and check it against SHA256SUMS\n# 2. Set RUNPOD_API_KEY in your user environment\n# 3. Add your SSH public key in RunPod\'s settings\n\noffrig status',
        },
        {
          title: 'Give agents the side-car',
          code: '# Register it with Claude Code at user scope\nclaude mcp add --scope user offrig -- <path>\\offrig-mcp.exe\n\n# Set the project\'s cap (human only)\ncd <project>\noffrig budget 15',
        },
      ],
    },
  ],
};
