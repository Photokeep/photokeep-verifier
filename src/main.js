import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./styles.css";

const app = document.querySelector("#app");

window.addEventListener("error", (event) => {
  document.body.innerHTML = `<pre style="white-space: pre-wrap; padding: 24px; color: #b42318;">Frontend load error: ${event.message}</pre>`;
});

window.addEventListener("unhandledrejection", (event) => {
  document.body.innerHTML = `<pre style="white-space: pre-wrap; padding: 24px; color: #b42318;">Frontend async error: ${event.reason}</pre>`;
});

app.innerHTML = `
  <main>
    <section class="top">
      <div>
        <h1>Photokeep Verifier</h1>
        <p>Select an encrypted original file, paste your key, and decrypt it locally for preview.</p>
      </div>
      <div class="status">Offline Mode</div>
    </section>

    <section class="trust">
      <span>No connection to Photokeep servers</span>
      <span>Files and keys are never uploaded</span>
      <span>Temporary decrypted files are cleaned up</span>
    </section>

    <section class="panel">
      <label>Encrypted file or Live Photo zip</label>
      <div class="file-row">
        <input id="file-path" readonly placeholder="Choose a .enc or .zip file" />
        <button id="choose-file" type="button">Choose File</button>
      </div>

      <label for="key">Your Key</label>
      <textarea id="key" spellcheck="false" placeholder="Open Photokeep app -> Account -> My Key, tap Copy, then paste it here."></textarea>

      <div class="actions">
        <button id="decrypt" type="button">Decrypt and Preview</button>
        <button id="clear" class="secondary" type="button">Clear Results</button>
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
  decryptButton.textContent = busy ? "Decrypting" : "Decrypt and Preview";
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
      unknown.textContent = "Decrypted, but this file type cannot be previewed directly in the current version.";
      section.appendChild(unknown);
    }

    const row = document.createElement("div");
    row.className = "result-actions";

    const save = document.createElement("button");
    save.className = "secondary";
    save.type = "button";
    save.textContent = "Save Decrypted File";
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
      openButton.textContent = "Open with System Player";
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
    setMessage("Unable to open file picker: " + String(err), true);
  }
});

decryptButton.addEventListener("click", async () => {
  const key = keyInput.value.trim();
  if (!selectedPath) {
    setMessage("Please choose an encrypted file first.", true);
    return;
  }
  if (!key) {
    setMessage("Please paste your key.", true);
    return;
  }

  setBusy(true);
  results.innerHTML = "";
  setMessage("Decrypting locally. Large videos may take a moment...");

  try {
    const response = await invoke("decrypt_file", { path: selectedPath, key });
    renderResults(response.items || []);
    setMessage("Decryption complete.");
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
    setMessage("Temporary results cleared.");
  } catch (err) {
    setMessage(String(err), true);
  }
});
