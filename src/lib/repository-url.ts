export function repositoryBrowserUrl(repositoryUrl: string): string | null {
  try {
    const parsedUrl = new URL(repositoryUrl);
    if (parsedUrl.protocol !== "https:" && parsedUrl.protocol !== "ssh:") {
      return null;
    }
    const authority = parsedUrl.protocol === "https:" ? parsedUrl.host : parsedUrl.hostname;
    const browserUrl = new URL(`https://${authority}`);
    browserUrl.pathname = parsedUrl.pathname.endsWith(".git") ? parsedUrl.pathname.slice(0, -4) : parsedUrl.pathname;
    return browserUrl.href;
  } catch {
    return null;
  }
}

export function repositoryPathBrowserUrl(repositoryUrl: string, commit: string, sourcePath: string, sourceIsDirectory: boolean): string | null {
  const browserUrl = repositoryBrowserUrl(repositoryUrl);
  if (browserUrl === null) {
    return null;
  }
  const parsedUrl = new URL(browserUrl);
  const repositoryPath = parsedUrl.pathname.replace(/\/$/u, "");
  if (parsedUrl.hostname === "github.com") {
    parsedUrl.pathname = `${repositoryPath}/${sourceIsDirectory ? "tree" : "blob"}/${commit}/${sourcePath}`;
  } else if (parsedUrl.hostname === "gitlab.com") {
    parsedUrl.pathname = `${repositoryPath}/-/${sourceIsDirectory ? "tree" : "blob"}/${commit}/${sourcePath}`;
  } else if (parsedUrl.hostname === "bitbucket.org") {
    parsedUrl.pathname = `${repositoryPath}/src/${commit}/${sourcePath}`;
  } else {
    return null;
  }
  return parsedUrl.href;
}
