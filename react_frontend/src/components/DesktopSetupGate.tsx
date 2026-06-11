import { useEffect, useState, type FormEvent, type ReactNode } from "react";

import {
  desktopOpenInBrowser,
  desktopSaveSetup,
  desktopSetupStatus,
  desktopStartStack,
  isDesktopRuntime,
} from "../desktop";

interface DesktopSetupGateProps {
  children: ReactNode;
}

type GateState = "checking" | "setup" | "starting" | "ready" | "error";

export default function DesktopSetupGate({ children }: DesktopSetupGateProps) {
  const [gateState, setGateState] = useState<GateState>(() => (
    isDesktopRuntime() ? "checking" : "ready"
  ));
  const [adminUsername, setAdminUsername] = useState("");
  const [adminPassword, setAdminPassword] = useState("");
  const [walletKey, setWalletKey] = useState("");
  const [walletKeyFile, setWalletKeyFile] = useState("");
  const [dataDir, setDataDir] = useState("");
  const [launcherUrl, setLauncherUrl] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    if (!isDesktopRuntime()) return;
    let active = true;

    async function bootstrap() {
      try {
        const status = await desktopSetupStatus();
        if (!active) return;
        setDataDir(status.data_dir);
        if (!status.configured) {
          setGateState("setup");
          return;
        }
        setGateState("starting");
        const launch = await desktopStartStack();
        if (!active) return;
        setLauncherUrl(launch.url);
        window.location.replace(launch.url);
      } catch (err) {
        if (!active) return;
        setError(err instanceof Error ? err.message : String(err));
        setGateState("error");
      }
    }

    bootstrap();
    return () => {
      active = false;
    };
  }, []);

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setError("");
    setGateState("starting");
    try {
      await desktopSaveSetup({
        adminUsername,
        adminPassword,
        walletKey: walletKey.trim() || undefined,
        walletKeyFile: walletKeyFile.trim() || undefined,
      });
      const launch = await desktopStartStack();
      setLauncherUrl(launch.url);
      window.location.replace(launch.url);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setGateState("setup");
    }
  };

  if (gateState === "ready") return <>{children}</>;

  if (gateState === "checking" || gateState === "starting") {
    return (
      <main className="desktop-setup-shell">
        <section className="desktop-setup-panel">
          <div className="section-kicker">Desktop</div>
          <h1>{gateState === "checking" ? "Checking setup..." : "Starting local services..."}</h1>
          {launcherUrl && (
            <button type="button" className="secondary-action" onClick={() => desktopOpenInBrowser(launcherUrl)}>
              Open in browser
            </button>
          )}
        </section>
      </main>
    );
  }

  if (gateState === "error") {
    return (
      <main className="desktop-setup-shell">
        <section className="desktop-setup-panel">
          <div className="section-kicker">Desktop</div>
          <h1>Local services could not start.</h1>
          <div className="error-box">{error}</div>
        </section>
      </main>
    );
  }

  return (
    <main className="desktop-setup-shell">
      <section className="desktop-setup-panel">
        <div className="section-kicker">First Run</div>
        <h1>Set up this desktop vault.</h1>
        {dataDir && <p className="desktop-setup-path">{dataDir}</p>}
        <form className="login-form" onSubmit={submit}>
          <label>
            <span>Admin username</span>
            <input value={adminUsername} onChange={(event) => setAdminUsername(event.target.value)} required />
          </label>
          <label>
            <span>Admin password</span>
            <input
              type="password"
              value={adminPassword}
              onChange={(event) => setAdminPassword(event.target.value)}
              minLength={12}
              required
            />
          </label>
          <label>
            <span>Wallet private key</span>
            <input
              type="password"
              value={walletKey}
              onChange={(event) => setWalletKey(event.target.value)}
              placeholder="0x..."
            />
          </label>
          <label>
            <span>Wallet key file path</span>
            <input
              value={walletKeyFile}
              onChange={(event) => setWalletKeyFile(event.target.value)}
              placeholder="/path/to/wallet-key"
            />
          </label>
          {error && <div className="error-box">{error}</div>}
          <button className="primary-action" type="submit">Save and start</button>
        </form>
      </section>
    </main>
  );
}
