import { useEffect, useState } from "react";
import Row from "./SettingsRow";
import Switch from "./SettingsSwitch";
import { appRestart, openPath, updateCheck, updateDownload, updateInstall, type UpdateCheck } from "../ipc";
import type { UpdateSettings } from "../settings";
import { fmtBytes, writeLastCheck } from "../updates";

/** Settings → Updates: is there a newer aiterm, and get it.
 *
 *  Nothing here is automatic past the check. Download and install are two
 *  clicks on purpose: the install raises a polkit password prompt, and a
 *  prompt that appears on its own is the kind of thing people rightly
 *  distrust. */
export default function UpdatesPane({ cfg, onChange, initial }: {
  cfg: UpdateSettings;
  onChange: (next: UpdateSettings) => void;
  /** A result the launch check already has, so the pane opens answered. */
  initial?: UpdateCheck | null;
}) {
  const [result, setResult] = useState<UpdateCheck | null>(initial ?? null);
  const [busy, setBusy] = useState<"check" | "download" | "install" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [downloaded, setDownloaded] = useState<string | null>(null);
  const [installed, setInstalled] = useState<string | null>(null);

  const check = async (prerelease = cfg.prerelease) => {
    setBusy("check");
    setError(null);
    setDownloaded(null);
    setInstalled(null);
    try {
      const r = await updateCheck(prerelease);
      setResult(r);
      writeLastCheck(Date.now());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  // Opening the pane without an answer in hand asks once; the launch check
  // usually already has one.
  useEffect(() => { if (!result) check(); /* eslint-disable-line react-hooks/exhaustive-deps */ }, []);

  const download = async () => {
    if (!result?.asset) return;
    setBusy("download");
    setError(null);
    try {
      setDownloaded(await updateDownload(result.asset));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const install = async () => {
    if (!downloaded) return;
    setBusy("install");
    setError(null);
    try {
      setInstalled(await updateInstall(downloaded));
      setDownloaded(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const installable = result?.package === "deb" || result?.package === "rpm";
  const when = result?.published_at ? new Date(result.published_at).toLocaleDateString() : "";

  return (
    <>
      <div className="sgroup">
        <div className="sgroup-title">This build</div>
        <div className="sgroup-rows">
          <Row label={`aiterm ${result?.current ?? __APP_VERSION__}`} desc={packageDesc(result?.package)}>
            <button className="set-recheck" disabled={busy !== null} onClick={() => check()}>
              {busy === "check" ? "Checking…" : "Check now"}
            </button>
          </Row>
        </div>
      </div>

      <div className="sgroup">
        <div className="sgroup-title">Newest release</div>
        <div className="sgroup-rows">
          {error && <div className="set-notice">{error}</div>}
          {!error && !result && <div className="sgroup-foot">Asking GitHub…</div>}
          {result && !result.newer && (
            <div className="sgroup-foot">
              You have the newest {result.prerelease ? "pre-release" : "release"} ({result.latest}
              {when && `, ${when}`}).
            </div>
          )}
          {result?.newer && (
            <>
              <Row
                label={`${result.latest} is available${result.prerelease ? " (pre-release)" : ""}`}
                desc={when ? `Published ${when}` : undefined}
              >
                <button className="set-recheck" onClick={() => openPath(result.url)}>Release page</button>
              </Row>
              {result.notes.trim() && (
                <pre className="diag-log upd-notes">{result.notes.trim()}</pre>
              )}
              <div className="diag-acts">
                {result.asset && !downloaded && !installed && (
                  <button className="set-recheck" disabled={busy !== null} onClick={download}>
                    {busy === "download"
                      ? "Downloading…"
                      : `Download ${result.asset.name} (${fmtBytes(result.asset.size)})`}
                  </button>
                )}
                {downloaded && installable && (
                  <button className="set-recheck" disabled={busy !== null} onClick={install}>
                    {busy === "install" ? "Installing…" : "Install (asks for your password)"}
                  </button>
                )}
                {installed && (
                  <button className="set-recheck" onClick={() => appRestart()}>
                    Restart aiterm
                  </button>
                )}
              </div>
              {result.package === "unpackaged" && (
                <div className="sgroup-foot">
                  This copy was not installed from a package, so there is nothing to install
                  over. Build it again, or install a package from the release page.
                </div>
              )}
              {result.package !== "unpackaged" && !result.asset && (
                <div className="sgroup-foot">
                  That release did not ship a {result.package} package. The release page has
                  what it did ship.
                </div>
              )}
              {downloaded && !installable && (
                <div className="sgroup-foot">
                  Saved to {downloaded}. Replace your current AppImage with it.
                </div>
              )}
              {downloaded && installable && (
                <div className="sgroup-foot">Saved to {downloaded}.</div>
              )}
              {installed && (
                <div className="set-notice">
                  {installed} is installed. Restarting closes every open terminal — finish
                  what is running first, or restart later.
                </div>
              )}
            </>
          )}
        </div>
      </div>

      <div className="sgroup">
        <div className="sgroup-title">Checking</div>
        <div className="sgroup-rows">
          <Row
            label="Check at launch"
            desc="Once a day, ask GitHub whether a newer release exists. Shows a mark in the top bar when one does; downloads nothing."
          >
            <Switch
              checked={cfg.checkOnLaunch}
              onChange={(on) => onChange({ ...cfg, checkOnLaunch: on })}
              label="Check for updates at launch"
            />
          </Row>
          <Row
            label="Include pre-releases"
            desc="Offer alpha and beta tags as well as the stable release."
          >
            <Switch
              checked={cfg.prerelease}
              onChange={(on) => { onChange({ ...cfg, prerelease: on }); check(on); }}
              label="Include pre-releases"
            />
          </Row>
        </div>
      </div>
    </>
  );
}

function packageDesc(kind: UpdateCheck["package"] | undefined): string {
  switch (kind) {
    case "deb": return "Installed as a .deb — updates go through apt.";
    case "rpm": return "Installed as an .rpm — updates go through dnf.";
    case "appimage": return "Running as an AppImage — updates are downloaded for you to swap in.";
    case "unpackaged": return "Built from source — updates can be checked, not installed.";
    default: return "";
  }
}
