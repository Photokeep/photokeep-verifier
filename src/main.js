import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./styles.css";

const app = document.querySelector("#app");

window.addEventListener("error", (event) => {
  document.body.innerHTML = `<pre style="white-space: pre-wrap; padding: 24px; color: #b42318;">前端加载错误：${event.message}</pre>`;
});

window.addEventListener("unhandledrejection", (event) => {
  document.body.innerHTML = `<pre style="white-space: pre-wrap; padding: 24px; color: #b42318;">前端异步错误：${event.reason}</pre>`;
});

app.innerHTML = `
  <main>
    <section class="top">
      <div>
        <h1>Photokeep 离线验证器</h1>
        <p>选择加密原始文件，粘贴自己的 AES key，在本机解密预览。</p>
      </div>
      <div class="status">离线模式</div>
    </section>

    <section class="trust">
      <span>不会连接 Photokeep 服务器</span>
      <span>文件和密钥不会上传</span>
      <span>退出后清理临时明文</span>
    </section>

    <section class="panel">
      <label>加密文件或 Live Photo zip</label>
      <div class="file-row">
        <input id="file-path" readonly placeholder="请选择 .enc 或 .zip 文件" />
        <button id="choose-file" type="button">选择文件</button>
      </div>

      <label for="key">AES key</label>
      <textarea id="key" spellcheck="false" placeholder="粘贴 base64 / hex / 32 字节原始密钥"></textarea>

      <div class="actions">
        <button id="decrypt" type="button">解密并预览</button>
        <button id="clear" class="secondary" type="button">清理结果</button>
      </div>

      <div id="message" class="message"></div>
      <div id="results" class="results"></div>
    </section>
  </main>
`;

const filePathInput = document.querySelector("#file-path");
const chooseFile = document.querySelector("#choose-file");
const decryptButton = document.querySelector("#decrypt");
const clearButton = document.querySelector("#clear");
const keyInput = document.querySelector("#key");
const message = document.querySelector("#message");
const results = document.querySelector("#results");

let selectedPath = "";

function previewSrc(path) {
  if (!path) return "";
  if (path.startsWith("asset:") || path.startsWith("http:") || path.startsWith("https:") || path.startsWith("data:")) {
    return path;
  }
  return convertFileSrc(path);
}

function setMessage(text, isError = false) {
  message.textContent = text;
  message.classList.toggle("error", isError);
}

function setBusy(busy) {
  decryptButton.disabled = busy;
  chooseFile.disabled = busy;
  decryptButton.textContent = busy ? "解密中" : "解密并预览";
}

function renderResults(items) {
  results.innerHTML = "";
  for (const item of items) {
    const section = document.createElement("section");
    section.className = "result";

    const title = document.createElement("div");
    title.className = "result-title";
    title.textContent = `${item.name} · ${item.kind}`;
    section.appendChild(title);

    if (item.kind === "image") {
      const img = document.createElement("img");
      img.src = previewSrc(item.preview_url);
      img.alt = item.name;
      section.appendChild(img);
    } else if (item.kind === "video") {
      const video = document.createElement("video");
      video.src = previewSrc(item.preview_url);
      video.controls = true;
      video.playsInline = true;
      section.appendChild(video);
    } else {
      const unknown = document.createElement("div");
      unknown.className = "unknown";
      unknown.textContent = "已解密，但当前版本无法直接预览此类型。";
      section.appendChild(unknown);
    }

    const row = document.createElement("div");
    row.className = "result-actions";

    const save = document.createElement("button");
    save.className = "secondary";
    save.type = "button";
    save.textContent = "保存解密文件";
    save.addEventListener("click", async () => {
      try {
        await invoke("save_result", { id: item.id });
      } catch (err) {
        setMessage(String(err), true);
      }
    });
    row.appendChild(save);

    if (item.kind === "video") {
      const openButton = document.createElement("button");
      openButton.className = "secondary";
      openButton.type = "button";
      openButton.textContent = "用系统播放器打开";
      openButton.addEventListener("click", async () => {
        try {
          await invoke("open_result", { id: item.id });
        } catch (err) {
          setMessage(String(err), true);
        }
      });
      row.appendChild(openButton);
    }

    if (item.note) {
      const note = document.createElement("span");
      note.className = "note";
      note.textContent = item.note;
      row.appendChild(note);
    }

    section.appendChild(row);
    results.appendChild(section);
  }
}

chooseFile.addEventListener("click", async () => {
  try {
    const path = await open({
      multiple: false,
      filters: [
        { name: "Photokeep encrypted files", extensions: ["enc", "zip"] },
        { name: "All files", extensions: ["*"] }
      ]
    });
    if (typeof path === "string") {
      selectedPath = path;
      filePathInput.value = path;
      results.innerHTML = "";
      setMessage("");
    }
  } catch (err) {
    setMessage("无法打开文件选择器：" + String(err), true);
  }
});

decryptButton.addEventListener("click", async () => {
  const key = keyInput.value.trim();
  if (!selectedPath) {
    setMessage("请先选择加密文件。", true);
    return;
  }
  if (!key) {
    setMessage("请粘贴 AES key。", true);
    return;
  }

  setBusy(true);
  results.innerHTML = "";
  setMessage("正在本机解密，较大的视频可能需要一点时间...");

  try {
    const response = await invoke("decrypt_file", { path: selectedPath, key });
    renderResults(response.items || []);
    setMessage("解密完成。");
  } catch (err) {
    setMessage(String(err), true);
  } finally {
    setBusy(false);
  }
});

clearButton.addEventListener("click", async () => {
  try {
    await invoke("clear_results");
    results.innerHTML = "";
    setMessage("已清理临时结果。");
  } catch (err) {
    setMessage(String(err), true);
  }
});
