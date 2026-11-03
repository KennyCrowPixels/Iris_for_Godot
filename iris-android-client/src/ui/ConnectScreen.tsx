import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, FlatList, Pressable, StyleSheet, Text, TextInput, View } from "react-native";
import { pair, ping } from "../api/client";
import { discoverySupported, startDiscovery } from "../api/discovery";
import type { Connection, DiscoveredHost } from "../api/types";
import { theme } from "../theme";

export default function ConnectScreen({ onConnected }: { onConnected: (connection: Connection) => void }) {
  const [hosts, setHosts] = useState<DiscoveredHost[]>([]);
  const [manualHost, setManualHost] = useState("");
  const [manualPort, setManualPort] = useState("1435");
  const [selected, setSelected] = useState<DiscoveredHost | null>(null);
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const scanning = useRef(false);

  useEffect(() => {
    if (!discoverySupported()) return;
    scanning.current = true;
    const stop = startDiscovery(
      (host) => setHosts((prev) => (prev.some((h) => h.host === host.host && h.port === host.port) ? prev : [...prev, host])),
      (message) => setError(message),
    );
    return () => {
      scanning.current = false;
      stop();
    };
  }, []);

  async function handlePair() {
    setError("");
    const host = selected?.host || manualHost.trim();
    const port = selected?.port || Number(manualPort);
    if (!host || !Number.isInteger(port)) {
      setError("Choose a discovered host or enter the desktop IP and port.");
      return;
    }
    if (!/^\d{6}$/.test(pin.trim())) {
      setError("Enter the 6-digit pairing PIN shown in Iris > Settings > Server.");
      return;
    }
    setBusy(true);
    try {
      await ping(host, port);
      const { token } = await pair(host, port, pin.trim());
      onConnected({ host, port, token, name: selected?.name });
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <View style={styles.container}>
      <Text style={styles.title}>Connect to Iris</Text>
      <Text style={styles.subtitle}>Open Iris on your desktop, go to Settings → Server, enable the companion server, and note the pairing PIN.</Text>

      <Text style={styles.section}>{discoverySupported() ? "Discovered on this Wi-Fi" : "Discovery needs a dev build — enter the host manually"}</Text>
      {discoverySupported() && hosts.length === 0 && <View style={styles.scanRow}><ActivityIndicator color={theme.accent} /><Text style={styles.muted}>  Scanning…</Text></View>}
      <FlatList
        data={hosts}
        keyExtractor={(item) => `${item.host}:${item.port}`}
        renderItem={({ item }) => (
          <Pressable style={[styles.host, selected?.host === item.host && styles.hostSelected]} onPress={() => { setSelected(item); setManualHost(item.host); setManualPort(String(item.port)); }}>
            <Text style={styles.hostName}>{item.name}</Text>
            <Text style={styles.muted}>{item.host}:{item.port}</Text>
          </Pressable>
        )}
        style={{ maxHeight: 160 }}
      />

      <Text style={styles.section}>Or enter manually</Text>
      <View style={styles.row}>
        <TextInput style={[styles.input, { flex: 3 }]} placeholder="192.168.1.x" placeholderTextColor={theme.muted} autoCapitalize="none" keyboardType="numbers-and-punctuation" value={manualHost} onChangeText={(value) => { setManualHost(value); setSelected(null); }} />
        <TextInput style={[styles.input, { flex: 1 }]} placeholder="1435" placeholderTextColor={theme.muted} keyboardType="number-pad" value={manualPort} onChangeText={setManualPort} />
      </View>

      <Text style={styles.section}>Pairing PIN</Text>
      <TextInput style={styles.input} placeholder="6 digits" placeholderTextColor={theme.muted} keyboardType="number-pad" maxLength={6} value={pin} onChangeText={setPin} />

      {error ? <Text style={styles.error}>{error}</Text> : null}

      <Pressable style={[styles.button, busy && styles.buttonDisabled]} disabled={busy} onPress={() => void handlePair()}>
        {busy ? <ActivityIndicator color={theme.accentInk} /> : <Text style={styles.buttonText}>Pair &amp; connect</Text>}
      </Pressable>
    </View>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, padding: 20, backgroundColor: theme.bg },
  title: { color: theme.text, fontSize: 24, fontWeight: "700", marginTop: 8 },
  subtitle: { color: theme.muted, fontSize: 13, marginTop: 8, lineHeight: 19 },
  section: { color: theme.text, fontSize: 13, fontWeight: "600", marginTop: 20, marginBottom: 8 },
  scanRow: { flexDirection: "row", alignItems: "center", paddingVertical: 8 },
  muted: { color: theme.muted, fontSize: 13 },
  host: { padding: 12, borderRadius: 10, borderWidth: 1, borderColor: theme.border, backgroundColor: theme.surface, marginBottom: 8 },
  hostSelected: { borderColor: theme.accent },
  hostName: { color: theme.text, fontSize: 15, fontWeight: "600" },
  row: { flexDirection: "row", gap: 8 },
  input: { backgroundColor: theme.surface, borderWidth: 1, borderColor: theme.border, borderRadius: 10, color: theme.text, paddingHorizontal: 14, paddingVertical: 12, fontSize: 15 },
  error: { color: theme.danger, marginTop: 14, fontSize: 13 },
  button: { backgroundColor: theme.accent, borderRadius: 12, paddingVertical: 15, alignItems: "center", marginTop: 22 },
  buttonDisabled: { opacity: 0.6 },
  buttonText: { color: theme.accentInk, fontWeight: "700", fontSize: 16 },
});
