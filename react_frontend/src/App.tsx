import { useCallback, useState } from "react";
import {
  BrowserRouter,
  Navigate,
  Route,
  Routes,
  useLocation,
  useNavigate,
  useParams,
} from "react-router-dom";

import "./App.css";
import ErrorBoundary from "./components/ErrorBoundary";
import Library from "./components/Library";
import LoginPanel from "./components/LoginPanel";
import UploadPanel from "./components/UploadPanel";
import { BRAND_IMAGE } from "./constants";
import useAuth from "./hooks/useAuth";
import type { AuthState } from "./types";

export { formatAttoTokens } from "./utils/format";

function VideoDetailRedirect() {
  const { videoId } = useParams<{ videoId?: string }>();
  return <Navigate to={videoId ? `/library/${encodeURIComponent(videoId)}` : "/library"} replace />;
}

function AppRoutes() {
  const [refreshKey, setRefreshKey] = useState(0);
  const navigate = useNavigate();
  const location = useLocation();
  const activeSection = location.pathname.startsWith("/manage")
    ? "manage"
    : location.pathname.startsWith("/upload")
      ? "upload"
      : location.pathname.startsWith("/login")
        ? "login"
        : "library";
  const handleAuthInvalid = useCallback(() => navigate("/library", { replace: true }), [navigate]);
  const { auth, login, logout: clearAuth } = useAuth(handleAuthInvalid);

  const handleLogin = async (nextAuth: AuthState) => {
    await login(nextAuth);
    navigate("/manage");
  };

  const logout = () => {
    clearAuth();
    navigate("/library");
  };

  const handleUploaded = () => {
    setRefreshKey((value) => value + 1);
    navigate("/manage");
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
            <button type="button" className={activeSection === "library" ? "active" : ""} onClick={() => navigate("/library")}>Library</button>
            {auth ? (
              <>
                <button type="button" className={activeSection === "manage" ? "active" : ""} onClick={() => navigate("/manage")}>Manage</button>
                <button type="button" className={activeSection === "upload" ? "active" : ""} onClick={() => navigate("/upload")}>Upload</button>
                <button type="button" onClick={logout}>Logout</button>
              </>
            ) : (
              <button type="button" className={activeSection === "login" ? "active" : ""} onClick={() => navigate("/login")}>Login</button>
            )}
          </nav>
        </div>
      </header>

      <main className="workspace-main">
        <Routes>
          <Route path="/" element={<Navigate to="/library" replace />} />
          <Route path="/library" element={<Library key={`public-${refreshKey}`} />} />
          <Route path="/library/:videoId" element={<Library key={`public-${refreshKey}`} />} />
          <Route path="/videos/:videoId" element={<VideoDetailRedirect />} />
          <Route
            path="/manage"
            element={auth ? <Library key={`admin-${refreshKey}`} admin /> : <Navigate to="/login" replace />}
          />
          <Route
            path="/manage/:videoId"
            element={auth ? <Library key={`admin-${refreshKey}`} admin /> : <Navigate to="/login" replace />}
          />
          <Route
            path="/upload"
            element={auth ? <UploadPanel onUploaded={handleUploaded} /> : <Navigate to="/login" replace />}
          />
          <Route
            path="/login"
            element={auth ? <Navigate to="/manage" replace /> : <LoginPanel onLogin={handleLogin} />}
          />
          <Route path="*" element={<Navigate to="/library" replace />} />
        </Routes>
      </main>
    </div>
  );
}

export default function App() {
  return (
    <ErrorBoundary>
      <BrowserRouter future={{ v7_relativeSplatPath: true, v7_startTransition: true }}>
        <AppRoutes />
      </BrowserRouter>
    </ErrorBoundary>
  );
}
