const MANIFEST_URL = "/downloads/manifest.json";
const SUPPORTED_PLATFORMS = new Set(["windows", "linux"]);

function detectPlatform() {
  const reportedPlatform = navigator.userAgentData?.platform ?? navigator.platform ?? "";
  const fingerprint = `${reportedPlatform} ${navigator.userAgent ?? ""}`.toLowerCase();

  if (fingerprint.includes("win")) return "windows";
  if (fingerprint.includes("linux") && !fingerprint.includes("android")) return "linux";
  return "unknown";
}

function isSafeReleasePath(value) {
  return typeof value === "string" && value.length > 0 && !value.startsWith("/") && !value.split("/").includes("..") && /^[a-zA-Z0-9._/-]+$/u.test(value);
}

function isPlatform(value) {
  return (
    value !== null &&
    typeof value === "object" &&
    SUPPORTED_PLATFORMS.has(value.id) &&
    typeof value.name === "string" &&
    typeof value.summary === "string" &&
    typeof value.format === "string" &&
    typeof value.architecture === "string" &&
    (value.file === null || isSafeReleasePath(value.file)) &&
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
    typeof value.release.notes !== "string" ||
    !Array.isArray(value.release.platforms) ||
    value.release.platforms.length !== 2 ||
    !value.release.platforms.every(isPlatform)
  ) {
    throw new Error("The release manifest is not valid.");
  }

  const ids = new Set(value.release.platforms.map((platform) => platform.id));
  if (ids.size !== 2 || ![...SUPPORTED_PLATFORMS].every((id) => ids.has(id))) {
    throw new Error("The release manifest must contain Windows and Linux exactly once.");
  }
  return value.release;
}

function formatBytes(bytes) {
  if (bytes === null) return "Size pending";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

function releaseUrl(path) {
  return `/downloads/${path.split("/").map(encodeURIComponent).join("/")}`;
}

function platformShortName(id) {
  return id === "windows" ? "Win" : "Linux";
}

function createDownloadCard(platform, detectedPlatform) {
  const card = document.createElement("article");
  card.className = "download-card";
  if (platform.id === detectedPlatform) card.classList.add("is-detected");

  const header = document.createElement("div");
  header.className = "download-card-header";

  const platformBlock = document.createElement("div");
  platformBlock.className = "download-platform";
  const mark = document.createElement("span");
  mark.className = "platform-mark";
  mark.textContent = platformShortName(platform.id);
  const copy = document.createElement("div");
  const heading = document.createElement("h3");
  heading.textContent = platform.name;
  const summary = document.createElement("p");
  summary.textContent = platform.summary;
  copy.append(heading, summary);
  platformBlock.append(mark, copy);
  header.append(platformBlock);

  if (platform.id === detectedPlatform) {
    const recommended = document.createElement("span");
    recommended.className = "recommended";
    recommended.textContent = "Recommended";
    header.append(recommended);
  }

  const meta = document.createElement("div");
  meta.className = "download-meta";
  for (const value of [platform.architecture, platform.format, formatBytes(platform.sizeBytes)]) {
    const item = document.createElement("span");
    item.textContent = value;
    meta.append(item);
  }

  const action = document.createElement("a");
  action.className = "download-action";
  const actionLabel = document.createElement("span");
  const actionIcon = document.createElement("small");
  actionIcon.setAttribute("aria-hidden", "true");

  if (platform.file !== null) {
    action.href = releaseUrl(platform.file);
    action.classList.add("available-file");
    actionLabel.textContent = `Download for ${platform.name}`;
    actionIcon.textContent = "↓";
  } else {
    action.setAttribute("aria-disabled", "true");
    actionLabel.textContent = `${platform.name} build coming soon`;
    actionIcon.textContent = "—";
    action.addEventListener("click", (event) => event.preventDefault());
  }
  action.append(actionLabel, actionIcon);

  card.append(header, meta, action);

  if (platform.sha256 !== null) {
    const checksum = document.createElement("div");
    checksum.className = "checksum";
    const label = document.createElement("span");
    label.textContent = "SHA-256";
    const digest = document.createElement("code");
    digest.textContent = platform.sha256;
    checksum.append(label, digest);
    card.append(checksum);
  }

  return card;
}

function renderRelease(release, detectedPlatform) {
  const list = document.querySelector("#download-list");
  const summary = document.querySelector("#release-summary");
  const note = document.querySelector("#release-note");
  const footerVersion = document.querySelector("#footer-version");
  if (!(list instanceof HTMLElement) || !(summary instanceof HTMLElement) || !(note instanceof HTMLElement) || !(footerVersion instanceof HTMLElement)) return;

  const orderedPlatforms = [...release.platforms].sort((left, right) => {
    if (left.id === detectedPlatform) return -1;
    if (right.id === detectedPlatform) return 1;
    return left.id === "windows" ? -1 : 1;
  });

  list.replaceChildren(...orderedPlatforms.map((platform) => createDownloadCard(platform, detectedPlatform)));
  const availableCount = release.platforms.filter((platform) => platform.file !== null).length;
  summary.textContent = availableCount > 0 ? `Version ${release.version} is ready for download.` : `Version ${release.version} page preview. Release packages are not mounted yet.`;
  footerVersion.textContent = `v${release.version}`;

  const noteHeading = document.createElement("strong");
  noteHeading.textContent = availableCount > 0 ? `Version ${release.version}` : "Preview mode";
  const noteCopy = document.createElement("p");
  noteCopy.textContent = release.notes;
  note.replaceChildren(noteHeading, noteCopy);

  const preferred = release.platforms.find((platform) => platform.id === detectedPlatform && platform.file !== null);
  const primary = document.querySelector("#primary-download");
  if (preferred !== undefined && primary instanceof HTMLAnchorElement) {
    primary.href = releaseUrl(preferred.file);
    const strong = primary.querySelector("strong");
    const small = primary.querySelector("small");
    if (strong !== null) strong.textContent = `Download for ${preferred.name}`;
    if (small !== null) small.textContent = `${preferred.architecture} · ${preferred.format} · ${formatBytes(preferred.sizeBytes)}`;
  }
}

function renderPlatformDetection(detectedPlatform) {
  const display = document.querySelector("#detected-platform span");
  const heroNote = document.querySelector("#hero-platform-note");
  if (!(display instanceof HTMLElement) || !(heroNote instanceof HTMLElement)) return;

  if (detectedPlatform === "windows") {
    display.textContent = "Windows detected";
    heroNote.textContent = "Windows detected — we’ll put the Windows installer first.";
  } else if (detectedPlatform === "linux") {
    display.textContent = "Linux detected";
    heroNote.textContent = "Linux detected — we’ll put the AppImage first.";
  } else {
    display.textContent = "Windows and Linux available";
    heroNote.textContent = "Choose between the Windows installer and Linux AppImage.";
  }
}

function renderManifestError() {
  const list = document.querySelector("#download-list");
  const summary = document.querySelector("#release-summary");
  if (!(list instanceof HTMLElement) || !(summary instanceof HTMLElement)) return;
  summary.textContent = "Release information is temporarily unavailable.";
  const card = document.createElement("article");
  card.className = "download-card";
  const heading = document.createElement("h3");
  heading.textContent = "Downloads unavailable";
  const copy = document.createElement("p");
  copy.textContent = "Please refresh the page in a moment.";
  card.append(heading, copy);
  list.replaceChildren(card);
}

async function initialize() {
  const detectedPlatform = detectPlatform();
  renderPlatformDetection(detectedPlatform);

  try {
    const response = await fetch(MANIFEST_URL, { cache: "no-store" });
    if (!response.ok) throw new Error(`Manifest request failed with ${response.status}.`);
    renderRelease(parseManifest(await response.json()), detectedPlatform);
  } catch (error) {
    console.error(error);
    renderManifestError();
  }
}

void initialize();
