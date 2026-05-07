import { useCallback, useEffect, useRef, useState } from "react";

import {
  getCurrentUser,
  hasCsrfCookie,
  logoutAdmin,
  refreshAdmin,
  subscribeAuthRefresh,
} from "../api/client";
import type { AuthState } from "../types";

export default function useAuth(onInvalid?: () => void) {
  const onInvalidRef = useRef(onInvalid);
  const authRef = useRef<AuthState | null>(null);
  const [auth, setAuth] = useState<AuthState | null>(null);

  useEffect(() => {
    onInvalidRef.current = onInvalid;
  }, [onInvalid]);

  const applyAuth = useCallback(async (nextAuth: AuthState) => {
    const currentUser = await getCurrentUser();
    const merged = { ...nextAuth, username: currentUser.username };
    authRef.current = merged;
    setAuth(merged);
  }, []);

  useEffect(() => subscribeAuthRefresh((nextAuth) => {
    if (nextAuth) {
      authRef.current = nextAuth;
      setAuth(nextAuth);
      return;
    }

    const hadAuth = !!authRef.current;
    authRef.current = null;
    setAuth(null);
    if (hadAuth) onInvalidRef.current?.();
  }), []);

  useEffect(() => {
    let active = true;

    async function restoreOrValidate() {
      if (!hasCsrfCookie()) {
        authRef.current = null;
        setAuth(null);
        return;
      }
      try {
        const nextAuth = await refreshAdmin();
        if (!active) return;
        await applyAuth(nextAuth);
      } catch {
        if (!active) return;
        authRef.current = null;
        setAuth(null);
      }
    }

    restoreOrValidate();

    return () => {
      active = false;
    };
  }, [applyAuth]);

  const login = useCallback(async (nextAuth: AuthState) => {
    await applyAuth(nextAuth);
  }, [applyAuth]);

  const logout = useCallback(() => {
    authRef.current = null;
    logoutAdmin().catch(() => {});
    setAuth(null);
  }, []);

  return { auth, login, logout };
}
