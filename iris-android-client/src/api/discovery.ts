// Zeroconf (mDNS) discovery of desktop Iris hosts advertising _iris-companion._tcp on the LAN.
// Requires a dev build (native module); in Expo Go this fails gracefully and the UI offers manual entry.

import type { DiscoveredHost } from "./types";

export function discoverySupported(): boolean {
  try {
    // Only present in a dev/prebuild binary that includes the native module.
    require("react-native-zeroconf");
    return true;
  } catch {
    return false;
  }
}

export function startDiscovery(onFound: (host: DiscoveredHost) => void, onError?: (message: string) => void): () => void {
  let zeroconf: any = null;
  try {
    const Zeroconf = require("react-native-zeroconf").default;
    zeroconf = new Zeroconf();
    zeroconf.on("resolved", (service: any) => {
      const addresses: string[] = Array.isArray(service?.addresses) ? service.addresses : [];
      const ipv4 = addresses.find((a) => /^\d+\.\d+\.\d+\.\d+$/.test(a));
      const host = ipv4 || service?.host;
      if (host && service?.port) {
        onFound({ name: service?.name || "Iris Companion", host, port: Number(service.port) });
      }
    });
    zeroconf.on("error", (error: unknown) => onError?.(String(error)));
    zeroconf.scan("iris-companion", "tcp", "local.");
  } catch (error) {
    onError?.(String(error));
  }
  return () => {
    try {
      zeroconf?.stop?.();
      zeroconf?.removeDeviceListeners?.();
    } catch {
      /* ignore */
    }
  };
}
