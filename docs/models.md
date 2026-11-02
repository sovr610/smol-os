# Local models

TermOS finds models on the Hugging Face Hub, tells you before downloading
whether one fits your machine, downloads it into the same cache Python's
`huggingface_hub` uses, and serves everything you downloaded on one
OpenAI-compatible endpoint. Agents use it as their `local` provider; any
other tool that speaks the OpenAI API can use it too.

- **Chat models** are GGUF files run by [llama.cpp](https://github.com/ggml-org/llama.cpp)'s
  `llama-server`, on the CPU or a GPU (Metal, Vulkan, CUDA, ROCm).
- **Embedding models** (sentence-transformers MiniLM, BGE, E5 and other
  BERT-family models) run inside TermOS with [candle](https://github.com/huggingface/candle),
  on the CPU, with nothing to install.
- **Anything else** (vLLM, transformers, a TPU VM) runs as a server you
  configure, which TermOS starts and stops the same way.

## Quick start

```sh
termos model recommend                  # what this machine can run
termos model search qwen                # GGUF models on the Hub
termos model info Qwen/Qwen3-4B-GGUF    # each file, and how it would fit
termos model pull Qwen/Qwen3-4B-GGUF    # the best file that fits
```

Then, in `~/.config/termos/config.toml`:

```toml
[agents.providers.local]
kind = "local"
```

Open an agent window (`C-b a`) with a profile that uses `provider = "local"`,
or check from the shell:

```sh
termos agent models local
```

The first request starts the model router in the background, and the first
chat with a GGUF model installs llama.cpp for this machine (through
[mise](languages.md), once). Both are then reused.

## Finding and downloading models

| Command | What it does |
| --- | --- |
| `termos model search QUERY [--all] [--limit N]` | Search the Hub, most downloaded first. GGUF repositories only, unless `--all`. |
| `termos model info REPO[:FILE]` | A repository's files, the estimate for each on this machine, and the model's shape (read from the GGUF header with a few small range requests). Nothing is downloaded. |
| `termos model pull REPO[:FILE] [--revision REV] [-y]` | Download a model. Shows the estimate and asks first. |
| `termos model list` | Downloaded models, with the ids the router serves them as. |
| `termos model rm REPO[:FILE]` | Remove a repository, or one file of it. |
| `termos model gc [--max SIZE]` | Remove interrupted downloads, files nothing uses any more and, over the size limit, the least recently used models. |
| `termos model recommend [QUERY]` | This machine's CPU, memory, GPUs and accelerators, how big a model fits, and with a query the Hub's matching models with the file that fits best. |

`REPO` is `org/name`. To choose a file, add it after a colon: a quantization
(`Qwen/Qwen3-4B-GGUF:q8_0`) or a path (`ggml-org/models:tinyllamas/stories260K.gguf`).
Without one, `pull` takes the file with the best placement (all on the GPU,
then split, then CPU) and, among those, the largest: more bits keep more
quality. Models split into parts (`-00001-of-00003.gguf`) are downloaded
whole.

Downloads **resume**: run the same command again after an interruption and it
continues where it stopped. Each file is checked against the Hub's SHA-256
(or git checksum, for small files) and deleted if it does not match. A pull
pins the commit the revision pointed to, so a branch that moves meanwhile
never mixes files.

**Gated and private models** need a Hugging Face token. Store it once and
point the config at it:

```sh
termos secret set huggingface
```

```toml
[models]
token = "keyring:huggingface"
```

`HF_TOKEN` works too. The token is sent only to the Hub.

### The cache

Models go where `huggingface_hub` puts them, so Python tools and TermOS share
downloads: `[models] cache`, else `$HF_HUB_CACHE`, else `$HF_HOME/hub`, else
`~/.cache/huggingface/hub`. `HF_ENDPOINT` points TermOS at a mirror.

`[models] cache-limit = "100G"` is the size `termos model gc` keeps the cache
under. TermOS never deletes models on its own.

## How the estimate works

Before any download, TermOS adds up:

- **the weights:** the file sizes;
- **the KV cache:** layers × context × KV heads × (key + value size) × 2
  bytes, from the GGUF header (or `config.json`), for `[models] context`
  tokens (4096 by default, or the model's own limit if smaller);
- **overhead:** 256 MiB plus 5% of the weights;

and compares the total, plus 10% headroom, with free GPU memory and free
RAM. A model then runs **on the GPU**, **split** (as many layers on the GPU
as fit, the rest on the CPU), **on the CPU**, or **does not fit**. On Apple
Silicon and other unified-memory machines, the GPU uses system memory.

A rule of thumb the estimate agrees with: Q4_K_M files take about 0.6 GB per
billion parameters, Q8_0 about 1.1 GB.

## The model router

```sh
termos models serve [--port N] [--socket PATH]   # in the foreground
termos models status                              # address, models, what is loaded
termos models stop                                # stop it and every model
termos models setup                               # install llama.cpp now
```

The router listens on `127.0.0.1` (a free port unless `--port` or
`[models] port` says otherwise) and, with `--socket`, on a Unix socket too. It
speaks the OpenAI API:

| Endpoint | |
| --- | --- |
| `GET /v1/models` | every downloaded model (with `termos.loaded`, `termos.kind`, `termos.size`) |
| `POST /v1/chat/completions` | streamed (`"stream": true`) or not |
| `POST /v1/completions` | |
| `POST /v1/embeddings` | embedding models |
| `GET /health` | no token needed |

Every other request needs the router's token as `Authorization: Bearer …`.
The token changes each time the router starts. The router writes its address
and token to `models.json` in TermOS's runtime folder (`$XDG_RUNTIME_DIR/termos/`
on Linux), readable only by you, and removes it when it stops. Agents read it
from there; so can your scripts:

```sh
f="$XDG_RUNTIME_DIR/termos/models.json"
url=$(jq -r .url "$f"); token=$(jq -r .token "$f")
curl "$url/chat/completions" -H "authorization: Bearer $token" \
  -H 'content-type: application/json' \
  -d '{"model": "Qwen/Qwen3-4B-GGUF", "messages": [{"role": "user", "content": "Hi"}]}'
```

**Model ids** are the repository when it holds one model, and
`repository:quantization` (lowercase) or `repository:path` when it holds
several. `termos model list` shows them.

**Models start on their first request** and stay loaded. When loading one
would pass the memory budget (`[models] memory-budget`, by default 90% of RAM
plus GPU memory), the least recently used ones are stopped first. Each GGUF
model gets its own `llama-server` on a local port, with its own key passed in
its environment (not on its command line, where other users could read it);
the router refuses to use a llama-server that does not check it.

## llama.cpp

TermOS uses llama.cpp's official release for your machine, installed by
mise on first use:

| `[models] llama-cpp` | Used when (`auto`) | Release builds exist for |
| --- | --- | --- |
| `metal` | Apple Silicon | macOS arm64 |
| `vulkan` | a GPU and the Vulkan loader are present (not a Raspberry Pi) | Linux x86_64/arm64, Windows x86_64 |
| `cpu` | otherwise | every platform |
| `cuda` | never automatically | Windows x86_64 |
| `rocm` | never automatically | Windows x86_64 (HIP) |

Vulkan runs on the NVIDIA and AMD drivers alone, so it is what `auto` picks
for a GPU. llama.cpp publishes **no CUDA or ROCm builds for Linux**, and its
Windows CUDA build also needs CUDA's runtime libraries (`cudart` and cuBLAS:
llama.cpp's separate `cudart-…` download, or the CUDA Toolkit, on `PATH`)
before you set `llama-cpp = "cuda"`. On Linux, NVIDIA and AMD cards can also
use a llama-server you build yourself:

```toml
[models]
llama-server = "/opt/llama.cpp/build/bin/llama-server"   # e.g. built with -DGGML_CUDA=ON
```

`TERMOS_LLAMA_SERVER` does the same for one run.

## Embedding models

A repository with `config.json` and `.safetensors` weights is served as an
embedding model with candle: BERT-family models such as
`sentence-transformers/all-MiniLM-L6-v2`, `BAAI/bge-small-en-v1.5` or
`intfloat/e5-small-v2`. Vectors are mean-pooled over the tokens and
normalized, as sentence-transformers does. Only the files candle needs are
downloaded (not the ONNX, OpenVINO or PyTorch copies).

```sh
termos model pull sentence-transformers/all-MiniLM-L6-v2
curl "$url/embeddings" -H "authorization: Bearer $token" -H 'content-type: application/json' \
  -d '{"model": "sentence-transformers/all-MiniLM-L6-v2", "input": ["hello", "world"]}'
```

## Other servers: vLLM, transformers, TPUs

For a big NVIDIA or AMD GPU, vLLM serves many requests faster than llama.cpp.
TermOS can set it up in a Python environment of its own (with
[uv](https://docs.astral.sh/uv/), through mise) and print the config to add:

```sh
termos models sidecar vllm --model Qwen/Qwen3-8B
```

Any OpenAI-compatible server can be configured the same way:

```toml
[models.servers.vllm]
command = ["/home/me/.local/share/termos/models/sidecar/vllm/bin/vllm", "serve", "{model}",
           "--host", "127.0.0.1", "--port", "{port}", "--served-model-name", "{repo}"]
env = { VLLM_API_KEY = "{token}" }
models = ["Qwen/Qwen3-8B"]     # repositories it serves (pull them first)
start-timeout = 900            # seconds to wait for it to answer
```

In `command` and `env`: `{model}` is the downloaded snapshot's folder,
`{repo}` the repository, `{port}` a free port, `{token}` a key the router
sends it, and `{cache}` the cache folder. `HF_HUB_CACHE` is set to the cache
and `HF_HUB_OFFLINE=1`, so the server uses what TermOS downloaded.

## Accelerators, and what they cannot do

| Hardware | What runs | Limits |
| --- | --- | --- |
| NVIDIA GPU | llama.cpp (Vulkan; CUDA on Windows or your own CUDA build); the vLLM sidecar | Free VRAM is read through NVML, else `nvidia-smi`. |
| AMD GPU | llama.cpp (Vulkan; HIP on Windows or your own ROCm build); vLLM's ROCm build as a configured server | Free VRAM is read from sysfs. The sidecar command installs vLLM's CUDA build. |
| Intel / other GPUs | llama.cpp's Vulkan build | Free memory is often unknown: TermOS then plans for the CPU. |
| Apple Silicon | llama.cpp with Metal | Unified memory: the GPU shares RAM. |
| Raspberry Pi 5 / 4 | llama.cpp on the CPU | Small models only: up to ~3B parameters at Q4 on 8 GB. The Pi's GPU is not used. |
| Coral Edge TPU (USB or PCIe) | **No language models.** | It runs only small int8 TensorFlow Lite models compiled for it (image and audio classifiers), through Google's own runtime. TermOS detects it and says so. |
| Hailo-8 / 8L | **No language models.** | They run compiled HEF models, mostly vision, through Hailo's own runtime. |
| Hailo-10H | not through TermOS's router | It runs some small language models through Hailo's own GenAI software, which TermOS does not integrate. If that software serves an OpenAI-compatible API, add it as an `openai-compatible` agent provider. |
| Cloud TPU (on a TPU VM) | the vLLM sidecar, with vLLM's TPU support | llama.cpp does not use TPUs, and TPUs exist only on Google Cloud TPU VMs. |

`termos model recommend` lists what it found and these notes for it;
`termos debug hw` shows the raw readings.

## Configuration

```toml
[models]
context = 4096           # tokens of context llama.cpp gets
llama-cpp = "auto"       # auto | cpu | vulkan | cuda | rocm | metal
port = 0                 # the router's port; 0 picks a free one
# cache = "~/.cache/huggingface/hub"
# cache-limit = "100G"
# token = "keyring:huggingface"
# llama-server = "/path/to/llama-server"
# memory-budget = "24G"
```

Sizes take `K`, `M`, `G` and `T` (powers of 1024) or a number of bytes.

## Troubleshooting

- **The router's log** is `models.log.<date>` in TermOS's log folder
  (`~/.local/state/termos/log/` on Linux). A backend that fails to start has
  its last lines in the error the request gets.
- **"llama.cpp publishes no … build"**: pick another `llama-cpp` build, or set
  `llama-server`.
- **A gated model** answers 401/403: accept its license on the Hub, then set
  the token as above.
- **Two routers?** `termos models status` shows the one agents use (the
  newest endpoint file).
