import { useState } from "react";

import { loginAdmin, requestErrorMessage } from "../api/client";
import { BRAND_IMAGE } from "../constants";

export default function LoginPanel({ onLogin }) {
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const submit = async (event) => {
    event.preventDefault();
    setLoading(true);
    setError("");
    try {
      const auth = await loginAdmin({ username, password });
      onLogin(auth);
    } catch (err) {
      setError(requestErrorMessage(err, "Login failed"));
    } finally {
      setLoading(false);
    }
  };

  return (
    <section className="login-card">
      <div className="login-grid">
        <div>
          <div className="section-kicker">Admin</div>
          <h1>Sign in to manage uploads.</h1>
          <form onSubmit={submit} className="login-form">
            <label>
              <span>Username</span>
              <input value={username} onChange={(event) => setUsername(event.target.value)} disabled={loading} />
            </label>
            <label>
              <span>Password</span>
              <input type="password" value={password} onChange={(event) => setPassword(event.target.value)} disabled={loading} />
            </label>
            {error && <div className="error-box">{error}</div>}
            <button className="primary-action" type="submit" disabled={loading}>
              {loading ? "Signing in..." : "Sign in"}
            </button>
          </form>
        </div>
        <div className="login-brand-panel" aria-hidden="true">
          <img src={BRAND_IMAGE} alt="" />
        </div>
      </div>
    </section>
  );
}
