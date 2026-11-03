// Persisted connection/session state. Stores the paired host + token so the phone reconnects silently
// after the first PIN pairing. The token is scoped to the desktop session and revocable from Iris.

import AsyncStorage from "@react-native-async-storage/async-storage";
import { useCallback, useEffect, useState } from "react";
import type { Connection } from "../api/types";

const STORAGE_KEY = "iris.connection";

export function useSession() {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const raw = await AsyncStorage.getItem(STORAGE_KEY);
        if (raw) setConnection(JSON.parse(raw));
      } catch {
        /* ignore corrupt storage */
      } finally {
        setLoaded(true);
      }
    })();
  }, []);

  const connect = useCallback(async (next: Connection) => {
    setConnection(next);
    await AsyncStorage.setItem(STORAGE_KEY, JSON.stringify(next)).catch(() => {});
  }, []);

  const disconnect = useCallback(async () => {
    setConnection(null);
    await AsyncStorage.removeItem(STORAGE_KEY).catch(() => {});
  }, []);

  return { connection, loaded, connect, disconnect };
}
