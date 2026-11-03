import { StatusBar } from "expo-status-bar";
import { ActivityIndicator, StyleSheet, View } from "react-native";
import { SafeAreaProvider, SafeAreaView } from "react-native-safe-area-context";
import ChatScreen from "./src/ui/ChatScreen";
import ConnectScreen from "./src/ui/ConnectScreen";
import { useSession } from "./src/state/useSession";
import { theme } from "./src/theme";

export default function App() {
  const { connection, loaded, connect, disconnect } = useSession();

  return (
    <SafeAreaProvider>
      <SafeAreaView style={styles.root} edges={["top", "bottom"]}>
        <StatusBar style="light" />
        {!loaded ? (
          <View style={styles.center}><ActivityIndicator color={theme.accent} /></View>
        ) : connection ? (
          <ChatScreen connection={connection} onDisconnect={() => void disconnect()} />
        ) : (
          <ConnectScreen onConnected={(next) => void connect(next)} />
        )}
      </SafeAreaView>
    </SafeAreaProvider>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: theme.bg },
  center: { flex: 1, alignItems: "center", justifyContent: "center", backgroundColor: theme.bg },
});
