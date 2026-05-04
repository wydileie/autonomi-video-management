import { useCallback, useState } from "react";

import "./App.css";
import Library from "./components/Library";
import LoginPanel from "./components/LoginPanel";
import UploadPanel from "./components/UploadPanel";
import { BRAND_IMAGE } from "./constants";
import useAuth from "./hooks/useAuth";

export { formatAttoTokens } from "./utils/format";

export default function App() {
  const [tab, setTab] = useState("library");
  const [refreshKey, setRefreshKey] = useState(0);
  const handleAuthInvalid = useCallback(() => setTab("library"), []);
  const { auth, login, logout: clearAuth } = useAuth(handleAuthInvalid);

  const handleLogin = (nextAuth) => {
    login(nextAuth);
    setTab("manage");
  };

  const logout = () => {
    clearAuth();
    setTab("library");
  };

  const handleUploaded = () => {
    setRefreshKey((value) => value + 1);
    setTab("manage");
  };

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand">
          <img className="brand-image" src={BRAND_IMAGE} alt="AutVid: Autonomi Video Vault" />
          <div className="brand-summary">
            <p>Smooth, adaptive video streaming powered by Autonomi.</p>
          </div>
        </div>
        <div className="topbar-actions">
          {auth && <span className="user-pill">{auth.username || "Admin"}</span>}
          <nav aria-label="Primary">
            <button type="button" className={tab === "library" ? "active" : ""} onClick={() => setTab("library")}>Library</button>
            {auth ? (
              <>
                <button type="button" className={tab === "manage" ? "active" : ""} onClick={() => setTab("manage")}>Manage</button>
                <button type="button" className={tab === "upload" ? "active" : ""} onClick={() => setTab("upload")}>Upload</button>
                <button type="button" onClick={logout}>Logout</button>
              </>
            ) : (
              <button type="button" className={tab === "login" ? "active" : ""} onClick={() => setTab("login")}>Login</button>
            )}
          </nav>
        </div>
      </header>

      <main className="workspace-main">
        {tab === "upload" && auth && <UploadPanel token={auth.access_token} onUploaded={handleUploaded} />}
        {tab === "manage" && auth && <Library key={`admin-${refreshKey}`} admin token={auth.access_token} />}
        {tab === "login" && !auth && <LoginPanel onLogin={handleLogin} />}
        {tab === "library" && <Library key={`public-${refreshKey}`} />}
      </main>
    </div>
  );
}
