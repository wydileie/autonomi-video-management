import { useCallback, useEffect, useState } from "react";

import {
  getAdminCatalogs,
  publishAdminCatalogs,
  requestErrorMessage,
} from "../api/client";
import type { AdminCatalogs } from "../types";

export function useAdminCatalogs(admin: boolean) {
  const [catalogs, setCatalogs] = useState<AdminCatalogs | null>(null);
  const [catalogPublishing, setCatalogPublishing] = useState(false);
  const [catalogCopied, setCatalogCopied] = useState("");
  const [catalogError, setCatalogError] = useState("");

  const loadCatalogs = useCallback(async () => {
    if (!admin) return;
    try {
      const data = await getAdminCatalogs();
      setCatalogs(data);
      setCatalogError("");
    } catch (err) {
      setCatalogError(requestErrorMessage(err, "Could not load catalog addresses."));
    }
  }, [admin]);

  useEffect(() => {
    void loadCatalogs();
  }, [loadCatalogs]);

  const republishCatalogs = useCallback(async () => {
    setCatalogPublishing(true);
    setCatalogError("");
    setCatalogCopied("");
    try {
      const data = await publishAdminCatalogs();
      setCatalogs(data);
    } catch (err) {
      setCatalogError(requestErrorMessage(err, "Catalog publish failed."));
    } finally {
      setCatalogPublishing(false);
    }
  }, []);

  const copyAddress = useCallback(async (label: string, address?: string | null) => {
    if (!address) return;
    try {
      await navigator.clipboard.writeText(address);
      setCatalogCopied(label);
    } catch {
      setCatalogError("Could not copy the catalog address.");
    }
  }, []);

  return {
    catalogCopied,
    catalogError,
    catalogPublishing,
    catalogs,
    copyAddress,
    loadCatalogs,
    republishCatalogs,
  };
}
