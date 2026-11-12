import type { IconName } from './icons'
import type { Text } from './i18n'

// The words of the page, in both languages, by section. A claim about what needy does today links its evidence; what
// is planned says so (`status: 'vision'`), so the page never presents a plan as a capability.

export type Status = 'works' | 'partly' | 'vision'

export const STATUS_TEXT: Record<Status, Text> = {
  works: { en: 'Works today', zh: '现在可用' },
  partly: { en: 'Partly there', zh: '部分可用' },
  vision: { en: 'Vision', zh: '设想' },
}

const EVIDENCE = 'https://github.com/needy-dev/needy/blob/main/docs/ai/references/architecture/2026-11-09-closed-loop-results-evidence.md'

export interface Capability {
  icon: IconName
  title: Text
  text: Text
  status: Status
  /** The section that tells more. */
  more: string
}

/** What needy is for, as capabilities. */
export const CAPABILITIES: Capability[] = [
  {
    icon: 'mingcute:git-branch-line',
    title: { en: 'Separate thinking from doing', zh: '思考与执行分离' },
    text: { en: 'The harness thinks in a stateless actor; its commands work on executors.', zh: 'harness 在无状态的 actor 里思考，命令在 executor 上执行。' },
    status: 'works',
    more: 'separation',
  },
  {
    icon: 'mingcute:route-line',
    title: { en: 'Seamless exec routing and policy', zh: '无感的命令路由与策略' },
    text: { en: 'Every exec and connection is caught and decided by rules. Nothing to change in the harness.', zh: '每次 exec 和连接都被捕获并按规则判定，harness 不用改。' },
    status: 'works',
    more: 'decisions',
  },
  {
    icon: 'mingcute:shield-line',
    title: { en: 'Isolation at the level you need', zh: '按需选择隔离级别' },
    text: { en: 'microVM, container or a VM on a Mac: one profile, four drivers.', zh: 'microVM、容器或 Mac 上的虚拟机：一份 profile，四种 driver。' },
    status: 'works',
    more: 'isolation',
  },
  {
    icon: 'mingcute:fingerprint-line',
    title: { en: 'A machine identity from a recipe', zh: '由 recipe 定义的机器身份' },
    text: { en: 'The actor looks like the machine a recipe describes.', zh: 'actor 看起来就是 recipe 描述的那台机器。' },
    status: 'works',
    more: 'identity',
  },
  {
    icon: 'mingcute:chart-network-line',
    title: { en: 'See how an agent system works', zh: '看清 agent 系统如何运作' },
    text: { en: 'Every decision is a typed event; needy report sums up a run.', zh: '每次判定都是带类型的事件，needy report 汇总一次运行。' },
    status: 'works',
    more: 'decisions',
  },
  {
    icon: 'mingcute:fire-line',
    title: { en: 'Chaos for agents', zh: '面向 agent 的混沌工程' },
    text: { en: 'Seeded faults today; suites that test harnesses next.', zh: '按 seed 的故障已可用；检验 harness 的故障套件在规划中。' },
    status: 'partly',
    more: 'chaos',
  },
  {
    icon: 'mingcute:laptop-line',
    title: { en: 'Small devices, remote machines', zh: '小设备 + 远程机器' },
    text: { en: 'A laptop with just the client drives an agent that works elsewhere.', zh: '只装客户端的笔记本，驱动在别处干活的 agent。' },
    status: 'works',
    more: 'cluster',
  },
  {
    icon: 'mingcute:cube-line',
    title: { en: 'Teleport workloads', zh: '负载"传送"' },
    text: { en: 'Spin up a macOS pool or a render farm as executors.', zh: '按需拉起 macOS 集群或渲染农场作为 executor。' },
    status: 'vision',
    more: 'future',
  },
]

export interface Section {
  id: string
  nav: Text
  kicker?: Text
  title: Text
  body: Text[]
  status?: Status
}

export const SECTIONS: Record<string, Section> = {
  why: {
    id: 'why',
    nav: { en: 'Why Needy', zh: '为什么是 Needy' },
    title: { en: 'Needy runs the harness apart from the work', zh: '把 harness 和干活的地方分开' },
    body: [
      {
        en: 'Agent harnesses such as Claude Code, Codex and opencode do two very different jobs in one process. They talk to a model, and they run whatever the model asks for: `cargo test`, `npm install`, `xcodebuild`. The first job mostly waits on the network. The second needs CPU, disk, a toolchain, and some care about what it is allowed to touch.',
        zh: 'Claude Code、Codex、opencode 这些 agent harness，其实在一个进程里同时干着两件事：一边跟模型聊，一边把模型让它跑的命令都跑了，比如 `cargo test`、`npm install`、`xcodebuild`。聊天这件事很轻，大部分时间都在等网络；跑命令就不一样了，要 CPU、要磁盘、要工具链，还得有人看着它别乱动东西。',
      },
      {
        en: 'Needy splits the two. The harness runs in a small sandbox we call the actor. Each command it starts is caught, checked against your rules, and run on an executor, which can be a container, a microVM or a real Mac. The harness itself does not need to change.',
        zh: 'Needy 做的事情就是把这两件事拆开。harness 放进一个很小的沙箱里，我们管它叫 actor；它发出的每条命令都会先被拦下来，按你写好的规则过一遍，再丢到 executor 上去跑。executor 可以是一个容器、一台 microVM，也可以直接是你的 Mac。harness 这边什么都不用改。',
      },
    ],
  },
  harness: {
    id: 'harness',
    nav: { en: 'Agent harness', zh: 'Agent harness' },
    title: { en: 'An agent harness is a loop', zh: 'Agent harness 其实就是个循环' },
    body: [
      {
        en: 'Watch a harness work for a minute and it turns out to be a loop: send the conversation and the available tools to a model, get back a tool call, run it, hand back the output, and repeat until the task is done. Most of the loop is waiting. The expensive and risky part is the tool call in the middle.',
        zh: '看 harness 干一会儿活就明白了，它其实就是个循环：把对话和能用的工具发给模型，模型回一个「去调用这个工具」，harness 照做，把结果交回去，然后再来一轮，直到任务做完。这个循环大部分时间都在等模型；真正花钱、也真正可能出事的，是中间那一步「调用工具」。',
      },
    ],
  },
  separation: {
    id: 'separation',
    nav: { en: 'Actor and executor', zh: '拆开的好处' },
    title: { en: 'Why the two live apart', zh: '为什么要拆开' },
    body: [
      {
        en: 'Once the loop and its tools live in different places, each can sit where it makes sense. The actor is small and keeps nothing, so it starts in a fraction of a second, close to the model it calls. Executors hold the workspace and the toolchains, so they can live on the machine with the right CPU, OS or license.',
        zh: '拆开之后，两边就能各自待在最合适的地方。actor 很小，也不存任何东西，不到一秒就能拉起来，所以可以放得离模型近一点；executor 存着工作区和工具链，那就放到 CPU、系统或授权都对得上的那台机器上。',
      },
      {
        en: 'A run has one actor and, by default, one executor. Everything that crosses between them is recorded, so you can read afterwards what the agent actually did.',
        zh: '默认一次运行就是一个 actor 加一个 executor。两边之间来来回回的每一件事都会记下来，事后翻一翻就知道 agent 到底干了什么。',
      },
    ],
  },
  isolation: {
    id: 'isolation',
    nav: { en: 'VMs and containers', zh: '虚拟机与容器' },
    title: { en: 'Four ways to run the actor', zh: 'actor 可以跑在这几种环境里' },
    body: [
      {
        en: "Not every host can run every kind of sandbox, so the actor has four drivers. On a Linux machine with KVM, use a Firecracker microVM. Without KVM, a container works, with runc or gVisor. On a Mac, use a VM through Virtualization.framework, or Apple's own `container`. They differ in how much of a real machine they can pretend to be; the table below has the details.",
        zh: '不是每台机器都能跑每一种沙箱，所以 actor 准备了四种 driver。有 KVM 的 Linux 上用 Firecracker microVM；没有 KVM，就用容器，runc 和 gVisor 都行；在 Mac 上，可以走 Virtualization.framework 起虚拟机，也可以直接用 Apple 自家的 `container`。它们的差别主要在于能把自己装成一台真机装到什么程度，下面的表里写得很清楚。',
      },
    ],
  },
  identity: {
    id: 'identity',
    nav: { en: 'Machine identity', zh: '机器身份' },
    title: { en: 'The program sees the machine a recipe describes', zh: '想让程序以为自己在哪台机器上，都可以' },
    body: [
      {
        en: 'Some programs behave differently depending on the machine they think they are on: a cloud SDK reads DMI strings, a build script reads `/proc/cpuinfo`, a license check looks at the hostname. A recipe decides what that machine looks like. With Firecracker, the actor presents the recipe all the way down to CPUID and PCI IDs.',
        zh: '有些程序会先看看自己在哪台机器上，再决定怎么跑：云厂商的 SDK 会去读 DMI，构建脚本会去读 `/proc/cpuinfo`，有的授权检查还会看主机名。recipe 就是用来定义「这台机器长什么样」的。用 Firecracker 的话，连 CPUID 和 PCI ID 都能照着 recipe 来。',
      },
    ],
  },
  decisions: {
    id: 'decisions',
    nav: { en: 'Exec and network', zh: '命令与网络' },
    title: { en: 'Rules decide where every command goes', zh: '每条命令去哪，规则说了算' },
    body: [
      {
        en: "Every exec in the actor is caught before it starts: by the `binfmt_needy` kernel module in a VM, or by shims in a container. The daemon walks the profile's rules and picks where the command runs: an executor, the actor itself, or nowhere. The harness still sees an ordinary process, with its output and its exit code.",
        zh: 'actor 里每一次 exec，在真正启动之前都会被拦住：虚拟机里靠 `binfmt_needy` 这个内核模块，容器里靠 shim。daemon 拿着 profile 里的规则一条条对，决定这条命令该去哪：某个 executor、actor 自己，或者干脆不让跑。harness 那边看到的还是一个普普通通的进程，输出和退出码都跟平时一样。',
      },
      {
        en: 'Network connections go through the gateway, which applies the allowlist in the same way. Hover a command below to see the decision for it.',
        zh: '网络连接也一样，都要经过 gateway，按白名单放行。把鼠标放到下面的命令上，就能看到每一条是怎么判的。',
      },
    ],
  },
  files: {
    id: 'files',
    nav: { en: 'Files', zh: '文件' },
    title: { en: 'Inject files, keep files: both work', zh: '注入文件和持久文件也没问题' },
    body: [
      {
        en: 'The actor is thrown away at the end of every run, so anything that should survive has to live somewhere else. The workspace usually lives on an executor and is mounted into the actor. A login or a cache can be copied in when the run starts and mirrored back when it ends.',
        zh: 'actor 每次跑完就直接扔掉了，所以想留下来的东西得放在别处。工作区一般放在 executor 上，再挂进 actor 里用；登录状态、缓存这类东西，可以在开跑的时候拷进去，结束的时候再同步回来。',
      },
    ],
  },
  chaos: {
    id: 'chaos',
    nav: { en: 'Chaos', zh: '混沌工程' },
    title: { en: "Agents are black boxes. Let's do some chaos engineering.", zh: 'Agent 太黑箱了，让我们做点混沌工程吧' },
    status: 'partly',
    body: [
      {
        en: 'Agents are mostly tested on the happy path. Real machines are less polite: disks return EIO, hosts refuse connections, CPUs throttle when they run hot. Needy injects those failures on purpose, from a seed, so the same run fails the same way twice and you can compare how different harnesses cope.',
        zh: '现在测 agent，基本都是在一切顺利的情况下测的。可真机器没那么好说话：磁盘会突然报 EIO，对面主机会拒绝连接，CPU 一热就降频。Needy 可以故意把这些故障按 seed 注入进去，同一次运行重放两遍，坏的地方一模一样，这样就能拿不同的 harness 来比一比，看谁扛得住。',
      },
      {
        en: 'Exec, file, network and sensor faults work today. What we want next is a set of fault suites that any harness can be run through.',
        zh: '命令、文件、网络、传感器这几类故障现在已经能用了。接下来想做一套故障测试集，任何 harness 都可以拉进来跑一遍。',
      },
    ],
  },
  config: {
    id: 'config',
    nav: { en: 'Configuration', zh: '配置' },
    title: { en: 'You only write two kinds of file', zh: '只需要写两种文件' },
    body: [
      {
        en: 'There are two kinds of file. A profile describes a run: which actor driver to use, which executors exist, and the rules for commands, files, the network and faults. A recipe describes the machine the actor pretends to be. Profiles can extend each other, and needy rejects unknown keys, so a typo fails loudly instead of being quietly ignored.',
        zh: '要写的文件就两种。profile 管一次运行：用哪种 actor driver、有哪些 executor，还有命令、文件、网络、故障各自的规则。recipe 管 actor 要装成的那台机器。profile 之间可以继承；写了 needy 不认识的键会直接报错，不会被悄悄吞掉，所以手滑打错一个字段名也能第一时间发现。',
      },
      {
        en: 'The studio lets you paste a profile, pick values from dropdowns, and see the run it would set up.',
        zh: '在 Profile 工作台里，可以直接粘一份 profile 进去，在下拉菜单里挑值，马上就能看到它会搭出一个什么样的运行。',
      },
    ],
  },
  cluster: {
    id: 'cluster',
    nav: { en: 'Machines', zh: '多台机器' },
    title: { en: 'Bring in every machine you have', zh: '手边的机器都能接进来' },
    body: [
      {
        en: 'Machines join a control plane as runners. A runner dials out and keeps that connection open, so it needs no open port; that makes it easy to add a Mac under a desk or a Windows box behind NAT. Profiles then place the actor and the executors on runners by name or by label.',
        zh: '机器以 runner 的身份接进控制平面。runner 是主动往外连的，连上之后一直挂着，自己不用开任何端口，所以桌子底下那台 Mac、躲在 NAT 后面的 Windows，都能很轻松地接进来。接进来之后，在 profile 里按名字或者标签，把 actor 和 executor 分到对应的机器上就行。',
      },
      {
        en: 'It works the other way round as well: a laptop that only has the needy client can pair with a control host once, and start runs there from then on.',
        zh: '反过来也可以：笔记本上只装一个 needy 客户端，跟控制主机配对一次，以后随时都能在那边发起运行。',
      },
    ],
  },
  future: {
    id: 'future',
    nav: { en: 'Where it goes', zh: '未来' },
    title: { en: 'What we want to build next', zh: '接下来想做的事' },
    status: 'vision',
    body: [
      {
        en: 'Nothing in this section is built yet. Where we are heading: a harness asks for the machines a task needs, such as a pool of Macs for an iOS build or a render farm for Blender, and they join as its executors for that run, while the harness stays in its own sandbox.',
        zh: '这一节写的都还没做出来。我们想往这个方向走：harness 需要什么机器就去要什么机器，比如 iOS 构建要一组 Mac，Blender 渲染要一个渲染农场，这些机器在这次运行里临时作为它的 executor 加进来，而 harness 自己还是好好待在沙箱里。',
      },
      {
        en: "At a larger scale, executors scheduled in groups, so many harnesses can run at once, the way DeepSeek's DSec runs sandboxes for agent RL.",
        zh: '再往大了做，就是把 executor 成组地调度起来，同时跑很多个 harness，就像 DeepSeek 的 DSec 给 agent 强化学习调度沙箱那样。',
      },
    ],
  },
  start: {
    id: 'start',
    nav: { en: 'Getting started', zh: '上手' },
    title: { en: 'Try it', zh: '动手试试' },
    body: [
      {
        en: 'Answer a few questions about where you want things to run, and the steps next to them change with your answers.',
        zh: '回答几个「东西想跑在哪」的问题，旁边的步骤会跟着你的选择变。',
      },
    ],
  },
}

export { EVIDENCE }

/**
 * How needy differs from the usual agent sandbox. The usual one: the harness and its tools share one sandbox, or the
 * harness calls a sandbox's API for each command (E2B, Modal, agentc; see the prior art in
 * docs/ai/references/architecture/2026-11-09-actor-executor-separation-design.md).
 */
export const COMPARISON: Array<{ aspect: Text, usual: Text, needy: Text }> = [
  { aspect: { en: 'Where the harness runs', zh: 'harness 跑在哪' }, usual: { en: 'In the sandbox, with its tools', zh: '和工具挤在同一个沙箱里' }, needy: { en: 'In its own actor, near the model', zh: '单独待在 actor 里，离模型近' } },
  { aspect: { en: 'How a command reaches the work', zh: '命令怎么跑到干活的地方' }, usual: { en: 'Runs where the harness is, or the harness calls an SDK', zh: '就地跑，或者 harness 自己去调 SDK' }, needy: { en: 'Caught in the kernel and routed by rules; the harness is unchanged', zh: '内核里直接拦下来按规则转走，harness 不用改' } },
  { aspect: { en: 'Policy', zh: '策略' }, usual: { en: 'Network on or off for the whole box', zh: '整个沙箱的网络，要么开要么关' }, needy: { en: 'Per command and per host; local services by name, their addresses kept outside', zh: '细到每条命令、每个主机；本地服务按名字访问，真实地址不进沙箱' } },
  { aspect: { en: 'Machines', zh: '机器' }, usual: { en: 'Linux containers', zh: 'Linux 容器' }, needy: { en: 'Containers, microVMs, a Mac or Windows box as executors', zh: '容器、microVM，Mac 和 Windows 也能当 executor' } },
  { aspect: { en: 'What the program sees', zh: '程序以为自己在哪' }, usual: { en: 'The image', zh: '就是那个镜像' }, needy: { en: 'A machine from a recipe: CPU, DMI, PCI', zh: 'recipe 里写的那台机器，CPU、DMI、PCI 都对得上' } },
  { aspect: { en: 'Seeing and breaking it', zh: '看得见、敢搞坏' }, usual: { en: 'Logs', zh: '日志' }, needy: { en: 'Typed events for every decision; seeded faults', zh: '每次判定都有事件记录；故障按 seed 注入' } },
]

/** What needy does not do, or does at a cost, today. From the README and the closed-loop results. */
export const LIMITS: Text[] = [
  { en: 'Experimental (0.0.1), built from a checkout.', zh: '还在实验阶段（0.0.1），要从源码自己编。' },
  { en: 'A routed exec costs about 1.5 ms to a Docker executor and 12 ms to a Mac over a tunnel (p50).', zh: '每次转发都有开销：转到 Docker executor 大约 1.5 ms，经隧道转到 Mac 大约 12 ms（p50）。' },
  { en: 'Container actors catch execs with shims, so programs added at run time are not routed.', zh: '容器版 actor 靠 shim 拦 exec，跑起来之后才装上的程序拦不到。' },
  { en: 'HTTPS is tunnelled, not inspected: the allowlist works by host.', zh: 'HTTPS 只做透传，不解密，所以白名单只能按主机来。' },
  { en: 'Commands kept in the actor, and terminal output, are not recorded.', zh: '留在 actor 里跑的命令，以及终端输出，不会被记下来。' },
  { en: 'Machines talk unencrypted; reach them through SSH or another tunnel.', zh: '机器之间的连接本身不加密，得走 SSH 或别的隧道。' },
]
