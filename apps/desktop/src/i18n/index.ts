import { en } from "./en";
import { ko } from "./ko";
import type { Locale, Messages } from "./types";

export const messages: Record<Locale, Messages> = { ko, en };
export type { Locale, Messages };
