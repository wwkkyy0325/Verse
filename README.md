# Verse

离线中文语音转文字。把音频或视频文件丢进去，得到带标点的字幕。

**识别全部在本机完成。** 不上传、不注册、不联网。整个程序里只有一条命令会发起
网络请求（`verse model fetch` 下载模型），只有一条命令会开端口（`verse serve`），
而它只听 `127.0.0.1`，并且不主动连任何地方。

[English](README.en.md)

## 三个入口，同一套管线

| 入口 | |
|---|---|
| **桌面窗口** | 拖文件进去就能用。一个页面：左边是引擎和这次处理过的文件，右边是转写正文。关掉窗口再打开，之前处理过的还在列表里 |
| **命令行** | `verse transcribe`。每条命令都支持 `--json`，一批文件传一个目录就行，失败是**不同的退出码**而不是一个笼统的非零 |
| **本地服务** | `verse serve`。让别的程序——包括 agent——把这台机器上的识别当成工作流里的一个节点来调 |

## 为什么用它

**标点是自带的，不是后处理。** 字幕出来就带逗号句号，这是默认引擎选 SenseVoice
的理由（标点 F1 在 83%–86%）。

**进度条不撒谎。** 别的工具要么转圈，要么编一个百分比。这里的总时长来自**本来
就要跑的那一遍 ffmpeg**——它的 `Duration:` 一直打在 stderr 上，只是以前被日志级别
挡住了。拿不到时长时（直播流、`Duration: N/A`）显示不确定态，**不编一个数字**。

**它会告诉你这次识别可能不可信。** 每个结果都带 `coverage` 和 `recovered`：
有多少非静音音频真的送到了识别器、以及这个比例低到不合理时是否整段重跑过。
一段"因为录音本来就短"的短转写，和一段"因为丢音频"的短转写，**光看文本是一模
一样的**——这两个字段是唯一的区别。

**不重复做功。** 结果按**内容哈希**缓存，同一个文件再处理是瞬时的；长文件中途
中断，下一次从断点接着跑。缓存就是本地文件，`--no-cache` 关掉它。

**下载器是能信的。** 断点续传（并且能识别出"服务器忽略了 Range 请求"这种会
**静默损坏文件**的情况，宁可重下也不追加）、写入 `.part` 再原子改名、**SHA-256
边下边校验**、镜像失败退避重试、**下之前先看磁盘够不够**、多个文件并发下载。

**并发数量是按这台机器算出来的，不是写死的。** 内存预算来自实际探测到的内存
（取四分之一），除以单个 worker 的实际成本；`/health` 会告诉你这个数字、它是
怎么来的、以及预算是**测出来的**还是**文档里的假设**。

## 实测

所有数字都在这台机器上量过（16 逻辑核 / 31.2 GiB / 纯 CPU），测法和"哪些数字
一开始是错的"都记在 `docs/llmwiki/tasks/` 里。

### 识别准确率

5 个数据集、5049 条语音，**按场景分开报，不报平均值**——因为差距本身就是结论：

| 场景 | 字错率 | 完全正确 | 标点 F1 |
|---|---|---|---|
| 日常对话 | **4.80%** | 63.4% | 83.0% |
| 会议 | 6.95% | 48.4% | **85.8%** |
| 纪录片 | 7.16% | 35.2% | 74.5% |
| 电话 | 8.42% | 28.8% | 76.5% |
| 直播带货 | 11.30% | 25.0% | 66.5% |

**单一数字没有意义。** 最高和最低差一倍以上；电话和直播难，是因为窄带，以及
在音乐上快速重叠的说话。任何"这个识别器大约 X%"的说法，都是在描述其中一行
而把其余藏起来。

### 两个引擎

| | SenseVoice-Small | Qwen3-ASR-0.6B |
|---|---|---|
| 体积 | **228 MB** | 982 MB |
| 速度 | **快** | 约慢 4 倍 |
| 标点 | **最好** | 好 |
| 准确率 | 好 | **在所有测过的场景上更好** |
| 专有名词表 | 不支持 | **支持** |
| 许可证 | FunASR 模型许可证 v1.1 | Apache-2.0 |

**默认是 SenseVoice**：标点最好、最快、体积只有四分之一。Qwen3 更准，但四倍的
时间和四倍的体积换来的差距不大——所以它是"你可以自己去下"的那个，不是默认。

### 并发

24 个任务，SenseVoice，关缓存。**总墙钟时间下降，但单个任务的延迟上升**——两个
数字必须一起看，只看一个都会误导：

| worker 数 | 总墙钟 | 单任务中位数 | 峰值内存 |
|---|---|---|---|
| 1 | 6.03 s | **249 ms** | **361 MB** |
| 2 | 3.51 s | 287 ms | 686 MB |
| 4 | 2.43 s | 375 ms | 1324 MB |
| 8 | **2.02 s** | 589 ms | 2619 MB |

一次只跑一个任务时，池子越小越快；二十几个任务一起时，池子越大越早结束。命令行
的 `-j` 是**显式选择**（60 个文件：`-j 1` 用 21 秒 / 347 MB，`-j 4` 用 10 秒 /
1245 MB），本地服务则按机器自动定。

### 试过，但没做

**生成式总结。** 摘录式摘要（挑原句）做出来过，看下来没有意义——它只能引用，
不能合并、不能下结论。换小 LLM 实测：0.5B 模型处理一段 5718 字的真实转写，
**预填充 279 秒、生成 65 秒，而且内容是错的**（把申请人和被申请人说反、编造了
原文里没有的提交方式）。

但那个 5 分 45 秒**测的是运行时而不是模型**——它的预填充是每个 token 恒定
50 毫秒的线性开销，说明没做批量矩阵乘。所以这**不是**"小模型在 CPU 上太慢"的
结论，而是"这个运行时不行"。完整的测法和结论在 `docs/llmwiki/tasks/llm-summary.md`。

## 安装

```
cargo build --release
```

**安装包自带 ffmpeg。** 一份 **LGPL** 构建，放在程序旁边——用 `PATH` 上那一份是
次选，因为自带的那份是发布时测过的那份；想用你自己的就设 `VERSE_FFMPEG`，它优先级
最高。（从源码运行才需要自己准备：`PATH` 上有一个，或者设 `VERSE_FFMPEG`。）

所有格式都是**调用** ffmpeg 处理的，从不链接它——它崩了、泄漏了，都传不到主进程。
哪一份构建、源码在哪，见 `THIRD_PARTY_NOTICES.md`。

还需要**模型**：`verse model fetch sensevoice` 拿默认那个，228 MB，VAD 模型随它一起。
`verse model list` 看已装了什么、占多少磁盘，`verse model remove <id>` 删掉一个，
`verse model clean` 清掉下了一半的。

**发布包只出 Windows。** macOS 和 Linux 都不做也不支持，两边理由不同，都写在
`.github/workflows/release.yml` 里：macOS 那边根本没有可用的 LGPL 版 ffmpeg；Linux
那边是**构建其实过了**——挂掉的是一个在 Windows 路径上写的测试，不是程序——但多一个
平台的打包配置和维护成本不值，所以按范围砍掉。

构建会下载一份预编译的 sherpa-onnx，网络慢的话看 `docs/llmwiki/design.md` §7.2。

## 使用

```console
$ verse model fetch sensevoice
$ verse transcribe meeting.m4a                        # 写到 <Documents>/Verse/meeting.srt
$ verse transcribe recordings/                        # 目录里每个录音，都进那个文件夹
$ verse transcribe meeting.m4a -o meeting.srt         # 或者指定位置
$ verse transcribe a.m4a --engine qwen3-asr           # 更准的那个
```

不加 `-o` 时，转写收在 Documents 下的 `Verse` 文件夹里。同一个文件跑两次是**覆盖**
而不是复制一份；同名但是两个不同录音，会自动加序号分开。`VERSE_OUTPUT` 可以改位置。

| 参数 | 作用 |
|---|---|
| `-o, --output <path>` | 一个文件，或者输入有多个时当目录 |
| `--json` | stdout 上只有一份 JSON；其余全在 stderr |
| `-j, --jobs <n>` | 同时处理几个文件。每个 worker 各加载一份模型 |
| `--hotwords <terms>` | 领域词表，让「球拍」不被听成「酒吧」。仅 Qwen3 |
| `--fail-fast` | 第一个失败就停，而不是跑完一批 |
| `--no-cache` | 即使有缓存也重新识别 |

## 从别的程序调用

`verse serve` 一直跑，让这台机器上任何程序通过 HTTP 要一次转写。它只听
`127.0.0.1`，不主动连接，所以不改变"什么东西离开你的机器"。

```console
$ verse serve
verse serve listening on http://127.0.0.1:17322
  token and port: .../Verse/serve.json
```

它写出带端口和 token 的 `serve.json`。读出来，然后：

```console
$ curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17322/health
$ curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d '{"input": "C:/audio/meeting.m4a"}' http://127.0.0.1:17322/jobs
{"version":1,"id":1,"state":"queued",...}
$ curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:17322/jobs/1
```

轮询到 `state` 变成 `done`，`result` 就是 `verse transcribe --json` 里
`.results[0]` 的同一份 JSON。`tools/verse-serve-client.py` 是一个只用标准库的完整
例子——没有 SDK、没有依赖——同一套请求用 PowerShell 的 `Invoke-RestMethod`
原样能跑。

接口也在界面的「关于」里，有一个**「复制给 agent」**按钮。

## 给驱动它的程序

数据在 **stdout**，诊断在 **stderr**，所以这样是可以的：

```console
$ verse transcribe recordings/ -o out/ --json 2>/dev/null | jq -r '.results[].text'
```

**一个文件和一批文件是同一种形状。** 单个文件就是只有一条的批次，`.results` 永远
是数组。调用方不需要按输入数量分支。

```json
{
  "version": 1, "engine": "sensevoice", "ok": false,
  "succeeded": 7, "failed": 1,
  "results": [{
    "input": "a.m4a", "output": "a.srt", "format": "srt", "ok": true,
    "segmentCount": 1, "coverage": 0.998, "recovered": false,
    "text": "...", "segments": [{"startMs": 0, "endMs": 1500, "text": "..."}],
    "error": null
  }]
}
```

每个字段永远存在，没有值时是 `null` 而不是缺字段。

**退出码**把需要不同处理方式的失败分开：

| 码 | 含义 | 怎么办 |
|---|---|---|
| 0 | 转写完成 | |
| 1 | 程序 bug | 报给我 |
| 2 | 命令行写错了 | 改调用方式 |
| 3 | 输入读不了或解不了码 | 换一个文件 |
| 4 | 没有可用的模型 | `verse model fetch <id>` |
| 5 | 识别引擎失败 | 重试 |
| 6 | 网络 | 重试，或手动准备模型 |
| 7 | 被取消 | |
| 8 | 结果写不出去 | 检查权限 |

## 不做的事

写在这里，免得它们悄悄溜进来：

- **实时字幕、翻译** —— 计划中，还没做。
- **总结** —— 试过了，见上面"试过，但没做"。
- **权重不进安装包** —— Qwen3 是 1 GB，而且它的 ONNX 权重来自第三方上传；内置
  会让每个用户为一个大多数人用不到的能力付费。
- **macOS 和 Linux** —— 不做，见上面安装一节。

## 开发

```
cargo test --workspace
cargo clippy --workspace --all-targets
```

386 个测试，clippy 干净。`docs/llmwiki/` 里是设计、任务日志和变更记录，从
`design.md` 开始。`AGENTS.md` 写了这个仓库的约定。

## 许可证

**MIT 或 Apache-2.0，任选其一**（`LICENSE-MIT` / `LICENSE-APACHE`）。

模型权重有自己的许可证，而且不随本项目分发；见 `THIRD_PARTY_NOTICES.md`。
