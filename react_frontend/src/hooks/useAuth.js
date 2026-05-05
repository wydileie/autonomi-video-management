import { useCallback, useEffect, useState } from "react";

import { getCurrentUser } from "../api/client";
import { AUTH_STORAGE_KEY } from "../constants";

export default function useAuth(onInvalid) {
  const [auth, setAuth] = useState(() => {
    const token = window.localStorage.getItem(AUTH_STORAGE_KEY);
    return token ? { access_token: token, username: "" } : null;
  });

  useEffect(() => {
    if (!auth?.access_token) return undefined;
    let active = true;

    getCurrentUser(auth.access_token)
      .then((currentUser) => {
        if (active) setAuth((current) => ({ ...current, username: currentUser.username }));
      })
      .catch(() => {
        window.localStorage.removeItem(AUTH_STORAGE_KEY);
        if (active) {
          setAuth(null);
          onInvalid?.();
        }
      });

    return () => {
      active = false;
    };
  }, [auth?.access_token, onInvalid]);

  const login = useCallback((nextAuth) => {
    window.localStorage.setItem(AUTH_STORAGE_KEY, nextAuth.access_token);
    setAuth(nextAuth);
  }, []);

  const logout = useCallback(() => {
    window.localStorage.removeItem(AUTH_STORAGE_KEY);
    setAuth(null);
  }, []);

  return { auth, login, logout };
}
