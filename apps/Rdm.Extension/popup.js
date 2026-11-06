const urlInput = document.querySelector("#url");
const submitButton = document.querySelector("#submit");
const statusMessage = document.querySelector("#status");

document.addEventListener("DOMContentLoaded", async () => {
  try {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (tab?.url && /^https?:\/\//i.test(tab.url)) {
      urlInput.value = tab.url;
      urlInput.focus();
      urlInput.select();
    }
  } catch (error) {
    statusMessage.dataset.state = "error";
    statusMessage.textContent = error.message || "Could not read the current tab URL.";
  }
});

submitButton.addEventListener("click", submitUrl);
urlInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    submitUrl();
  }
});

async function submitUrl() {
  submitButton.disabled = true;
  statusMessage.dataset.state = "";
  statusMessage.textContent = "Connecting to RDM...";
  try {
    const response = await chrome.runtime.sendMessage({
      type: "rdm:submit-url",
      url: urlInput.value.trim()
    });
    if (response?.accepted !== true) {
      throw new Error(response?.error || "RDM did not confirm receipt of the URL.");
    }
    statusMessage.dataset.state = "success";
    statusMessage.textContent = "URL sent to RDM.";
  } catch (error) {
    statusMessage.dataset.state = "error";
    statusMessage.textContent = error.message || "Could not connect to RDM.";
  } finally {
    submitButton.disabled = false;
  }
}
