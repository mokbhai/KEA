import { useCallback, useEffect, useState } from "react";

const MODIFIER_CODES = new Set([
  "MetaLeft",
  "MetaRight",
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "AltRight",
  "OSLeft",
  "OSRight",
]);

const NAMED_KEYS: Record<string, string> = {
  Space: "Space",
  Tab: "Tab",
  Enter: "Enter",
  Escape: "Escape",
  Backspace: "Backspace",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  Minus: "Minus",
  Equal: "Equal",
  BracketLeft: "BracketLeft",
  BracketRight: "BracketRight",
  Backslash: "Backslash",
  Semicolon: "Semicolon",
  Quote: "Quote",
  Comma: "Comma",
  Period: "Period",
  Slash: "Slash",
  Backquote: "Backquote",
};

export function codeToMainKey(code: string): string | null {
  if (MODIFIER_CODES.has(code)) return null;
  if (code.startsWith("Key") && code.length === 4) return code.slice(3);
  if (code.startsWith("Digit") && code.length === 6) return code.slice(5);
  if (/^F\d{1,2}$/.test(code)) return code;
  if (code.startsWith("Numpad") && code.length > 6) return code.slice(6);
  return NAMED_KEYS[code] ?? null;
}

export function buildAccelerator(event: KeyboardEvent): string | null {
  const mainKey = codeToMainKey(event.code);
  if (!mainKey) return null;

  const modifiers: string[] = [];
  if (event.metaKey || event.ctrlKey) modifiers.push("CommandOrControl");
  if (event.shiftKey) modifiers.push("Shift");
  if (event.altKey) modifiers.push("Alt");

  if (modifiers.length === 0) return null;
  return [...modifiers, mainKey].join("+");
}

export function parseAcceleratorParts(accelerator: string): string[] {
  return accelerator
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean);
}

const isMac =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPod|iPad/i.test(navigator.platform);

export function acceleratorPartLabel(part: string): string {
  switch (part) {
    case "CommandOrControl":
      return isMac ? "⌘" : "Ctrl";
    case "Cmd":
    case "Command":
      return "⌘";
    case "Control":
    case "Ctrl":
      return isMac ? "⌃" : "Ctrl";
    case "Shift":
      return "⇧";
    case "Alt":
    case "Option":
      return isMac ? "⌥" : "Alt";
    case "Space":
      return "Space";
    default:
      return part;
  }
}

type RecorderState = {
  capturing: boolean;
  pending: string | null;
  captureError: string | null;
  startCapture: () => void;
  cancelCapture: () => void;
  clearPending: () => void;
};

export function useHotkeyRecorder(): RecorderState {
  const [capturing, setCapturing] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);

  const startCapture = useCallback(() => {
    setPending(null);
    setCaptureError(null);
    setCapturing(true);
  }, []);

  const cancelCapture = useCallback(() => {
    setCapturing(false);
    setCaptureError(null);
  }, []);

  const clearPending = useCallback(() => {
    setPending(null);
    setCaptureError(null);
  }, []);

  useEffect(() => {
    if (!capturing) return;

    const onKeyDown = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();

      if (event.key === "Escape") {
        setCapturing(false);
        setCaptureError(null);
        return;
      }

      const mainKey = codeToMainKey(event.code);
      if (!mainKey) return;

      const hasModifier =
        event.metaKey || event.ctrlKey || event.shiftKey || event.altKey;
      if (!hasModifier) {
        setCaptureError("Include at least one modifier (⌘/Ctrl, Shift, or Alt).");
        return;
      }

      const accelerator = buildAccelerator(event);
      if (!accelerator) return;

      setPending(accelerator);
      setCaptureError(null);
      setCapturing(false);
    };

    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [capturing]);

  return {
    capturing,
    pending,
    captureError,
    startCapture,
    cancelCapture,
    clearPending,
  };
}
