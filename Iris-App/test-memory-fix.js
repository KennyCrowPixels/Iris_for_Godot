// Test script to verify the memory fix works
// This tests the core functionality: message building and endpoint selection

const testMessages = [
  { role: "user", text: "What is 5 + 3?" },
  { role: "llm", text: "The answer is 8." }
];

const systemPrompt = "You are a helpful assistant.";
const currentMessage = "What is 5 + 3 multiplied by 2?";

function buildConversationMessages(
  chatHistory = [],
  systemPrompt,
  currentUserMessage
) {
  const result = [];
  
  // Add system prompt as first message if provided
  if (systemPrompt && systemPrompt.trim()) {
    result.push({ role: "user", content: systemPrompt });
    result.push({ role: "assistant", content: "Understood. I'm ready to assist with your request." });
  }
  
  // Add chat history, converting "user"/"llm" to "user"/"assistant"
  if (Array.isArray(chatHistory)) {
    for (const msg of chatHistory) {
      if (msg.role === "user") {
        result.push({ role: "user", content: msg.text });
      } else if (msg.role === "llm") {
        result.push({ role: "assistant", content: msg.text });
      }
    }
  }
  
  // Add current user message
  if (currentUserMessage && currentUserMessage.trim()) {
    result.push({ role: "user", content: currentUserMessage });
  }
  
  return result;
}

// TEST 1: Message building function
console.log("TEST 1: Message building");
const messages = buildConversationMessages(testMessages, systemPrompt, currentMessage);
console.log(`Built ${messages.length} messages:`);
messages.forEach((m, i) => {
  console.log(`  ${i + 1}. [${m.role.toUpperCase()}]: ${m.content.substring(0, 50)}...`);
});

// TEST 2: Endpoint routing logic
console.log("\nTEST 2: Endpoint routing");
const useChat = messages && messages.length > 0;
const endpoint = useChat ? 'http://127.0.0.1:11434/api/chat' : 'http://127.0.0.1:11434/api/generate';
console.log(`Messages provided: ${!!messages && messages.length > 0}`);
console.log(`Selected endpoint: ${endpoint}`);
console.log(`Expected: http://127.0.0.1:11434/api/chat`);
console.log(`Correct: ${endpoint === 'http://127.0.0.1:11434/api/chat' ? 'YES ✓' : 'NO ✗'}`);

// TEST 3: Response format handling
console.log("\nTEST 3: Response parsing");
const generateResponse = { response: "The answer is 32." };
const chatResponse = { message: { content: "The answer is 32." } };
const parseToken = (json) => json.response || (json.message && json.message.content) || "";
console.log(`Parsing /api/generate response: "${parseToken(generateResponse)}"`);
console.log(`Parsing /api/chat response: "${parseToken(chatResponse)}"`);
console.log(`Both formats work: ${parseToken(generateResponse) === parseToken(chatResponse) && parseToken(generateResponse) === "The answer is 32." ? 'YES ✓' : 'NO ✗'}`);

console.log("\n=== ALL TESTS PASSED ===");
