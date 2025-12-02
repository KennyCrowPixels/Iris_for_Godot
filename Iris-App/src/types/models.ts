export type Role = "user" | "llm";

export type ChatMessage = { role: "user" | "llm"; text: string };
export type Artifact = { lang: string; filename?: string; content: string; ts?: number };

export type Tab = {
	id: number;
	title: string;
	type?: "chat" | "settings" | string;
	messages?: ChatMessage[];
	artifacts?: Artifact[];
};

export type TestModelResult = {
	success?: boolean;
	output?: string;
	[key: string]: any;
};

