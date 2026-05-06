import { useCallback, useEffect, useRef, useState } from "react";

import { getCurrentUser, logoutAdmin, refreshAdmin, subscribeAuthRefresh } from "../api/client";
import { AUTH_STORAGE_KEY } from "../constants";
import type { AuthState } from "../types";

export default function useAuth(onInvalid?: () => void) {
  const skipNextRefresh = useRef(false);
  const onInvalidRef = useRef(onInvalid);
  const authRef = useRef<AuthState | null>(null);
  const [auth, setAuth] = useState<AuthState | null>(() => {
    const token = window.localStorage.getItem(AUTH_STORAGE_KEY);
    const initialAuth = token ? { access_token: token, username: "" } : null;
    authRef.current = initialAuth;
    return initialAuth;
  });

  useEffect(() => {
    onInvalidRef.current = onInvalid;
  }, [onInvalid]);

  useEffect(() => {
    authRef.current = auth;
  }, [auth]);

  useEffect(() => subscribeAuthRefresh((nextAuth) => {
    if (nextAuth) {
      const refreshedAuth = {
        ...nextAuth,
        username: nextAuth.username || authRef.current?.username || "",
      };
      skipNextRefresh.current = false;
      authRef.current = refreshedAuth;
      setAuth(refreshedAuth);
      return;
    }

    const hadAuth = !!authRef.current?.access_token;
    skipNextRefresh.current = true;
    authRef.current = null;
    window.localStorage.removeItem(AUTH_STORAGE_KEY);
    setAuth(null);
    if (hadAuth) onInvalidRef.current?.();
  }), []);

  useEffect(() => {
    if (!auth?.access_token && skipNextRefresh.current) {
      skipNextRefresh.current = false;
      return undefined;
    }

    let active = true;

    async function restoreOrValidate() {
      const applyAuth = async (nextAuth: AuthState) => {
        const currentUser = await getCurrentUser(nextAuth.access_token);
        if (active) {
          setAuth({ ...nextAuth, username: currentUser.username });
        }
      };

      try {
        if (auth?.access_token) {
          await applyAuth(auth);
        } else {
          await applyAuth(await refreshAdmin());
        }
      } catch {
        if (auth?.access_token) {
          try {
            await applyAuth(await refreshAdmin());
            return;
          } catch {
            // Fall through to clearing stale local bearer state.
          }
        }
        window.localStorage.removeItem(AUTH_STORAGE_KEY);
        if (active) {
          authRef.current = null;
          setAuth(null);
          if (auth?.access_token) onInvalid?.();
        }
      }
    }

    restoreOrValidate();

    return () => {
      active = false;
    };
  }, [auth?.access_token, onInvalid]);

  const login = useCallback((nextAuth: AuthState) => {
    authRef.current = nextAuth;
    setAuth(nextAuth);
  }, []);

  const logout = useCallback(() => {
    skipNextRefresh.current = true;
    authRef.current = null;
    logoutAdmin().catch(() => {});
    setAuth(null);
  }, []);

  return { auth, login, logout };
}
