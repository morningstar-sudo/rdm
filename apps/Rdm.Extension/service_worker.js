const HOST_NAME = "com.rdm.download";

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "rdm-download-link",
      title: "Download link with RDM",
      contexts: ["link"]
    });
    chrome.contextMenus.create({
      id: "rdm-download-page",
      title: "Download this page with RDM",
      contexts: ["page"]
    });
  });
});

chrome.contextMenus.onClicked.addListener((info) => {
  const url = info.menuItemId === "rdm-download-link"
    ? info.linkUrl
    : info.pageUrl;
  sendToRdm(url).catch((error) => {
    console.error("RDM could not accept the URL:", error);
  });
});

chrome.downloads.onCreated.addListener((item) => {
  if (item.state !== "in_progress" || !/^https?:\/\//i.test(item.url)) {
    return;
  }
  handoffBrowserDownload(item);
});

chrome.commands.onCommand.addListener((command) => {
  if (command !== "download-current-page") {
    return;
  }
  chrome.tabs.query({ active: true, currentWindow: true }, (tabs) => {
    sendToRdm(tabs[0]?.url).catch((error) => {
      console.error("RDM could not accept the URL:", error);
    });
  });
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type !== "rdm:submit-url") {
    return false;
  }
  sendToRdm(message.url, false)
    .then((response) => sendResponse(response))
    .catch((error) => sendResponse({ error: error.message }));
  return true;
});

async function handoffBrowserDownload(item) {
  let paused = false;
  try {
    paused = await pauseBrowserDownload(item.id);
    if (!paused) {
      return;
    }
    const response = await sendToRdm(item.finalUrl || item.url, true, item.filename);
    if (!response.accepted) {
      try {
        await chrome.downloads.resume(item.id);
      } catch (error) {
        console.error(`Could not resume rejected browser download ${item.id}:`, error);
      }
      return;
    }
    await chrome.downloads.cancel(item.id);
    chrome.action.setBadgeText({ text: "RDM" });
    chrome.action.setBadgeBackgroundColor({ color: "#107c10" });
    setTimeout(() => chrome.action.setBadgeText({ text: "" }), 2500);
  } catch (error) {
    console.error(
      `Could not hand off browser download ${item.id}; keeping the browser download active:`,
      error
    );
    if (paused) {
      try {
        await chrome.downloads.resume(item.id);
      } catch (resumeError) {
        console.error(`Could not resume browser download ${item.id}:`, resumeError);
      }
    }
    chrome.action.setBadgeText({ text: "!" });
    chrome.action.setBadgeBackgroundColor({ color: "#c42b1c" });
    setTimeout(() => chrome.action.setBadgeText({ text: "" }), 4000);
  }
}

async function pauseBrowserDownload(downloadId) {
  try {
    await chrome.downloads.pause(downloadId);
    return true;
  } catch (error) {
    const [item] = await chrome.downloads.search({ id: downloadId });
    if (item?.state === "complete") {
      return false;
    }
    throw error;
  }
}

function sendToRdm(url, autoDownload = false, filename) {
  if (!url || !/^https?:\/\//i.test(url)) {
    return Promise.reject(new Error("Only HTTP and HTTPS links are supported."));
  }
  return new Promise((resolve, reject) => {
    let settled = false;
    const port = chrome.runtime.connectNative(HOST_NAME);
    port.onMessage.addListener((message) => {
      settled = true;
      port.disconnect();
      if (message?.error) {
        reject(new Error(message.error));
      } else if (typeof message?.accepted === "boolean") {
        resolve({ accepted: message.accepted });
      } else {
        reject(new Error("RDM returned an invalid response."));
      }
    });
    port.onDisconnect.addListener(() => {
      if (!settled) {
        reject(new Error(chrome.runtime.lastError?.message ?? "Could not connect to RDM."));
      }
    });
    port.postMessage({ url, autoDownload, filename });
  });
}
