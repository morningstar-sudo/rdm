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
  if (item.state !== "in_progress") {
    return;
  }
  const url = getHttpDownloadUrl(item);
  if (!url) {
    reportDownloadStatus(
      "Edge started a browser-only download that RDM cannot fetch. Keep it in Edge.",
      "!"
    );
    return;
  }
  handoffBrowserDownload(item, url);
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

async function handoffBrowserDownload(item, initialUrl) {
  let paused = false;
  try {
    paused = await pauseBrowserDownload(item.id);
    if (!paused) {
      reportDownloadStatus(
        `Edge finished "${item.filename}" before RDM could take over.`,
        "!"
      );
      return;
    }
    const [currentItem] = await chrome.downloads.search({ id: item.id });
    const url = getHttpDownloadUrl(currentItem) || initialUrl;
    const response = await sendToRdm(url, true, item.filename);
    if (!response.accepted) {
      try {
        await chrome.downloads.resume(item.id);
        reportDownloadStatus(
          `Kept "${item.filename}" in Edge.`,
          "E"
        );
      } catch (error) {
        console.error(`Could not resume rejected browser download ${item.id}:`, error);
        reportDownloadStatus(
          `Could not resume "${item.filename}" in Edge: ${error.message}`,
          "!"
        );
      }
      return;
    }
    await chrome.downloads.cancel(item.id);
    reportDownloadStatus(`Sent "${item.filename}" to RDM.`, "RDM", "#107c10");
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
        reportDownloadStatus(
          `RDM handoff failed and Edge could not resume "${item.filename}": ${resumeError.message}`,
          "!"
        );
      }
    }
    reportDownloadStatus(
      `Could not send "${item.filename}" to RDM: ${error.message}`,
      "!"
    );
  }
}

async function pauseBrowserDownload(downloadId) {
  const [item] = await chrome.downloads.search({ id: downloadId });
  if (!item || item.state !== "in_progress") {
    return false;
  }
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

function getHttpDownloadUrl(item) {
  if (!item) {
    return undefined;
  }
  return [item.finalUrl, item.url].find((url) => /^https?:\/\//i.test(url || ""));
}

function reportDownloadStatus(message, badge, color = "#c42b1c") {
  chrome.storage.local.set({
    lastDownloadStatus: {
      message,
      timestamp: Date.now()
    }
  });
  chrome.action.setBadgeText({ text: badge });
  chrome.action.setBadgeBackgroundColor({ color });
  chrome.action.setTitle({ title: message });
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
