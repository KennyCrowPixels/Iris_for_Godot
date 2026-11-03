import { useEffect, useMemo, useRef, useState } from "react";
import { ActivityIndicator, FlatList, Image, Pressable, StyleSheet, Switch, Text, TextInput, View } from "react-native";
import { createChat, getChat, listChats, submitTurn, waitForJob } from "../api/client";
import { pickCompressedImages } from "../api/images";
import type { ChatMessage, Connection } from "../api/types";
import { MobileVoiceRecognizer, cancelSpeech, speak } from "../voice/voice";
import { theme } from "../theme";

export default function ChatScreen({ connection, onDisconnect }: { connection: Connection; onDisconnect: () => void }) {
  const [tabId, setTabId] = useState<number | null>(null);
  const [title, setTitle] = useState("Iris");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [pendingImages, setPendingImages] = useState<string[]>([]);
  const [networkAllowed, setNetworkAllowed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [listening, setListening] = useState(false);
  const [speaking, setSpeaking] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");

  const recognizer = useMemo(() => new MobileVoiceRecognizer(), []);
  const speakOnDone = useRef(false);
  const stopRequested = useRef(false);
  const listRef = useRef<FlatList<ChatMessage>>(null);

  useEffect(() => {
    (async () => {
      try {
        const chats = await listChats(connection);
        if (chats.length > 0) {
          setTabId(chats[0].id);
          setTitle(chats[0].title);
        } else {
          const created = await createChat(connection, "Mobile chat");
          setTabId(created.id);
          setTitle(created.title);
        }
      } catch (failure) {
        setError(String(failure instanceof Error ? failure.message : failure));
      }
    })();
    return () => {
      recognizer.stop();
      cancelSpeech();
    };
  }, [connection]);

  useEffect(() => {
    if (tabId == null) return;
    void refreshMessages();
  }, [tabId]);

  async function refreshMessages() {
    if (tabId == null) return;
    try {
      const chat = await getChat(connection, tabId);
      setMessages(chat.messages || []);
      setTitle(chat.title || title);
      setTimeout(() => listRef.current?.scrollToEnd({ animated: true }), 50);
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure));
    }
  }

  function stopSpeaking() {
    cancelSpeech();
    setSpeaking(false);
  }

  async function send(viaVoice: boolean) {
    const text = input.trim();
    if (tabId == null || busy || (!text && pendingImages.length === 0)) return;
    const images = pendingImages;
    setMessages((prev) => [...prev, { role: "user", text: text || "(image)", images }]);
    setInput("");
    setPendingImages([]);
    setBusy(true);
    setError("");
    setStatus("Sending to desktop…");
    stopRequested.current = false;
    speakOnDone.current = viaVoice;
    try {
      const { jobId } = await submitTurn(connection, tabId, text, { networkAllowed, images });
      setStatus("Iris is working…");
      await waitForJob(connection, jobId, (job) => setStatus(job.status === "running" ? "Iris is working…" : job.status), () => stopRequested.current);
      await refreshMessages();
      if (speakOnDone.current) {
        const chat = await getChat(connection, tabId);
        const lastLlm = [...(chat.messages || [])].reverse().find((m) => m.role === "llm");
        if (lastLlm?.text) speak(lastLlm.text, { onStart: () => setSpeaking(true), onDone: () => setSpeaking(false) });
      }
    } catch (failure) {
      const message = String(failure instanceof Error ? failure.message : failure);
      if (message !== "cancelled") setError(message);
    } finally {
      setBusy(false);
      setStatus("");
    }
  }

  async function toggleMic() {
    if (listening) {
      recognizer.stop();
      return;
    }
    stopSpeaking();
    const started = await recognizer.start({
      onTranscript: (transcript) => setInput(transcript),
      onEnd: (finalText) => {
        setListening(false);
        if (finalText.trim()) {
          setInput(finalText);
          speakOnDone.current = true;
          setTimeout(() => void send(true), 60);
        }
      },
      onError: (message) => { setListening(false); setError(message); },
    });
    setListening(started);
  }

  async function attachImages() {
    try {
      const picked = await pickCompressedImages(4 - pendingImages.length);
      if (picked.length) setPendingImages((prev) => [...prev, ...picked].slice(0, 4));
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure));
    }
  }

  return (
    <View style={styles.container}>
      <View style={styles.header}>
        <View style={{ flex: 1 }}>
          <Text style={styles.title}>{title}</Text>
          <Text style={styles.muted}>{connection.host}:{connection.port}</Text>
        </View>
        <Pressable onPress={onDisconnect} hitSlop={8}><Text style={styles.link}>Disconnect</Text></Pressable>
      </View>

      <FlatList
        ref={listRef}
        data={messages}
        keyExtractor={(_, index) => String(index)}
        contentContainerStyle={{ padding: 14, gap: 10 }}
        renderItem={({ item }) => (
          <View style={[styles.bubble, item.role === "user" ? styles.userBubble : styles.llmBubble]}>
            <Text style={styles.role}>{item.role === "user" ? "You" : "Iris"}</Text>
            {!!item.text && <Text style={styles.bubbleText}>{item.text}</Text>}
            {Array.isArray(item.images) && item.images.length > 0 && (
              <View style={styles.thumbRow}>{item.images.map((src, i) => <Image key={i} source={{ uri: src }} style={styles.thumb} />)}</View>
            )}
          </View>
        )}
        onContentSizeChange={() => listRef.current?.scrollToEnd({ animated: true })}
      />

      {!!status && <View style={styles.statusRow}><ActivityIndicator color={theme.accent} size="small" /><Text style={styles.muted}>  {status}</Text>{busy && <Pressable onPress={() => { stopRequested.current = true; stopSpeaking(); }} hitSlop={8}><Text style={styles.stopLink}>  Stop</Text></Pressable>}</View>}
      {!!error && <Text style={styles.error}>{error}</Text>}

      {pendingImages.length > 0 && (
        <View style={styles.thumbRow}>{pendingImages.map((src, i) => <Image key={i} source={{ uri: src }} style={styles.thumbSmall} />)}</View>
      )}

      <View style={styles.networkRow}>
        <Text style={styles.muted}>Allow web search this turn</Text>
        <Switch value={networkAllowed} onValueChange={setNetworkAllowed} trackColor={{ true: theme.accent }} />
      </View>

      <View style={styles.composer}>
        <Pressable style={styles.iconBtn} onPress={() => void attachImages()} disabled={busy || pendingImages.length >= 4}><Text style={styles.icon}>+</Text></Pressable>
        <TextInput style={styles.composerInput} placeholder="Message Iris…" placeholderTextColor={theme.muted} value={input} onChangeText={setInput} multiline />
        {speaking && <Pressable style={[styles.iconBtn, styles.iconActive]} onPress={stopSpeaking}><Text style={styles.icon}>■</Text></Pressable>}
        <Pressable style={[styles.iconBtn, listening && styles.iconActive]} onPress={() => void toggleMic()}><Text style={styles.icon}>{listening ? "●" : "🎤"}</Text></Pressable>
        <Pressable style={[styles.sendBtn, busy && styles.buttonDisabled]} disabled={busy} onPress={() => void send(false)}><Text style={styles.sendText}>↑</Text></Pressable>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, backgroundColor: theme.bg },
  header: { flexDirection: "row", alignItems: "center", paddingHorizontal: 16, paddingVertical: 12, borderBottomWidth: 1, borderBottomColor: theme.border },
  title: { color: theme.text, fontSize: 17, fontWeight: "700" },
  muted: { color: theme.muted, fontSize: 12 },
  link: { color: theme.accent, fontSize: 13, fontWeight: "600" },
  stopLink: { color: theme.danger, fontSize: 13, fontWeight: "600" },
  bubble: { padding: 12, borderRadius: 12, borderWidth: 1, borderColor: theme.border, maxWidth: "92%" },
  userBubble: { alignSelf: "flex-end", backgroundColor: theme.userBubble },
  llmBubble: { alignSelf: "flex-start", backgroundColor: theme.surface },
  role: { color: theme.muted, fontSize: 11, marginBottom: 4, fontWeight: "600" },
  bubbleText: { color: theme.text, fontSize: 15, lineHeight: 21 },
  thumbRow: { flexDirection: "row", flexWrap: "wrap", gap: 6, marginTop: 8 },
  thumb: { width: 110, height: 110, borderRadius: 8 },
  thumbSmall: { width: 54, height: 54, borderRadius: 6 },
  statusRow: { flexDirection: "row", alignItems: "center", paddingHorizontal: 16, paddingVertical: 6 },
  error: { color: theme.danger, paddingHorizontal: 16, paddingVertical: 6, fontSize: 13 },
  networkRow: { flexDirection: "row", alignItems: "center", justifyContent: "space-between", paddingHorizontal: 16, paddingVertical: 4 },
  composer: { flexDirection: "row", alignItems: "flex-end", gap: 8, padding: 12, borderTopWidth: 1, borderTopColor: theme.border },
  composerInput: { flex: 1, maxHeight: 120, backgroundColor: theme.surface, borderWidth: 1, borderColor: theme.border, borderRadius: 10, color: theme.text, paddingHorizontal: 14, paddingVertical: 10, fontSize: 15 },
  iconBtn: { width: 44, height: 44, borderRadius: 10, borderWidth: 1, borderColor: theme.border, backgroundColor: theme.surface, alignItems: "center", justifyContent: "center" },
  iconActive: { backgroundColor: theme.accent, borderColor: theme.accent },
  icon: { color: theme.text, fontSize: 18 },
  sendBtn: { width: 44, height: 44, borderRadius: 10, backgroundColor: theme.accent, alignItems: "center", justifyContent: "center" },
  sendText: { color: theme.accentInk, fontSize: 22, fontWeight: "700" },
  buttonDisabled: { opacity: 0.6 },
});
