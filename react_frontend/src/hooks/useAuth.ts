import { useCallback, useEffect, useRef, useState } from "react";

import { getCurrentUser, logoutAdmin, refreshAdmin } from "../api/client";
import { AUTH_STORAGE_KEY } from "../constants";
import type { AuthState } from "../types";

export default function useAuth(onInvalid?: () => void) {
  const skipNextRefresh = useRef(false);
  const [auth, setAuth] = useState<AuthState | null>(() => {
    const token = window.localStorage.getItem(AUTH_STORAGE_KEY);
    return token ? { access_token: token, username: "" } : null;
  });

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
    setAuth(nextAuth);
  }, []);

  const logout = useCallback(() => {
    skipNextRefresh.current = true;
    logoutAdmin().catch(() => {});
    setAuth(null);
  }, []);

  return { auth, login, logout };
}
