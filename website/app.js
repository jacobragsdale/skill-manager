const MANIFEST_URL = "/downloads/manifest.json";
const PLATFORM_IDS = new Set(["windows", "linux"]);

function detectPlatform() {
  const reported = navigator.userAgentData?.platform ?? navigator.platform ?? "";
  const value = `${reported} ${navigator.userAgent ?? ""}`.toLowerCase();
  if (value.includes("win")) return "windows";
  if (value.includes("linux") && !value.includes("android")) return "linux";
  return "unknown";
}

function isSafePath(value) {
  return typeof value === "string" && value.length > 0 && !value.startsWith("/") && !value.split("/").includes("..") && /^[a-zA-Z0-9._/-]+$/u.test(value);
}

function isPlatform(value) {
  return (
    value !== null &&
    typeof value === "object" &&
    PLATFORM_IDS.has(value.id) &&
    typeof value.name === "string" &&
    typeof value.summary === "string" &&
    typeof value.format === "string" &&
    typeof value.architecture === "string" &&
    (value.file === null || isSafePath(value.file)) &&
    (value.sizeBytes === null || (Number.isInteger(value.sizeBytes) && value.sizeBytes > 0)) &&
    (value.sha256 === null || (typeof value.sha256 === "string" && /^[a-f0-9]{64}$/u.test(value.sha256)))
  );
}

function parseManifest(value) {
  if (
    value === null ||
    typeof value !== "object" ||
    value.schemaVersion !== 1 ||
    value.release === null ||
    typeof value.release !== "object" ||
    typeof value.release.version !== "string" ||
    !Array.isArray(value.release.platforms) ||
    value.release.platforms.length !== 2 ||
    !value.release.platforms.every(isPlatform)
  ) {
    throw new Error("Invalid release manifest.");
  }

  const ids = new Set(value.release.platforms.map((platform) => platform.id));
  if (ids.size !== 2 || ![...PLATFORM_IDS].every((id) => ids.has(id))) {
    throw new Error("The manifest must contain Windows and Linux.");
  }
  return value.release;
}

function formatBytes(bytes) {
  if (bytes === null) return null;
  const megabytes = bytes / (1024 * 1024);
  return `${megabytes < 10 ? megabytes.toFixed(1) : megabytes.toFixed(0)} MB`;
}

function releaseUrl(path) {
  return `/downloads/${path.split("/").map(encodeURIComponent).join("/")}`;
}

function createDownload(platform, detectedPlatform) {
  const item = document.createElement("article");
  item.className = "download-item";
  if (platform.id === detectedPlatform) item.classList.add("recommended");

  const heading = document.createElement("h3");
  heading.textContent = platform.name;
  const summary = document.createElement("p");
  summary.textContent = platform.summary;
  item.append(heading, summary);

  if (platform.file !== null) {
    const link = document.createElement("a");
    link.className = "download-link";
    link.href = releaseUrl(platform.file);
    link.textContent = `Download for ${platform.name}`;
    const details = document.createElement("span");
    details.className = "file-details";
    const size = formatBytes(platform.sizeBytes);
    details.textContent = [platform.architecture, platform.format, size].filter(Boolean).join(" / ");
    link.append(details);
    item.append(link);
  } else {
    const unavailable = document.createElement("span");
    unavailable.className = "download-unavailable";
    unavailable.textContent = "Not available yet";
    item.append(unavailable);
  }

  if (platform.sha256 !== null) {
    const checksum = document.createElement("div");
    checksum.className = "checksum";
    checksum.textContent = `SHA-256: ${platform.sha256}`;
    item.append(checksum);
  }
  return item;
}

function renderPlatformNote(platform) {
  const note = document.querySelector("#platform-note");
  if (!(note instanceof HTMLElement)) return;
  if (platform === "windows") note.textContent = "Windows detected. The Windows download is listed first.";
  else if (platform === "linux") note.textContent = "Linux detected. The Linux download is listed first.";
  else note.textContent = "Windows is listed first. A Linux AppImage is also available.";
}

function renderRelease(release, detectedPlatform) {
  const list = document.querySelector("#download-list");
  const summary = document.querySelector("#release-summary");
  const footer = document.querySelector("#footer-version");
  if (!(list instanceof HTMLElement) || !(summary instanceof HTMLElement) || !(footer instanceof HTMLElement)) return;

  const platforms = [...release.platforms].sort((left, right) => {
    if (left.id === detectedPlatform) return -1;
    if (right.id === detectedPlatform) return 1;
    return left.id === "windows" ? -1 : 1;
  });
  list.replaceChildren(...platforms.map((platform) => createDownload(platform, detectedPlatform)));

  const available = platforms.some((platform) => platform.file !== null);
  summary.textContent = available ? `Current version: ${release.version}` : `Version ${release.version}. No public files have been added yet.`;
  footer.textContent = `Version ${release.version}`;
}

function renderError() {
  const list = document.querySelector("#download-list");
  const summary = document.querySelector("#release-summary");
  if (list instanceof HTMLElement) list.textContent = "Downloads are temporarily unavailable.";
  if (summary instanceof HTMLElement) summary.textContent = "Could not load release information.";
}

async function initialize() {
  const detectedPlatform = detectPlatform();
  renderPlatformNote(detectedPlatform);
  try {
    const response = await fetch(MANIFEST_URL, { cache: "no-store" });
    if (!response.ok) throw new Error(`Manifest request failed with ${response.status}.`);
    renderRelease(parseManifest(await response.json()), detectedPlatform);
  } catch (error) {
    console.error(error);
    renderError();
  }
}

void initialize();
