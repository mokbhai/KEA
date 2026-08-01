import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import { getSetting, setSetting } from "./api";

export type Theme = "light" | "dark";
export type ThemePreference = "system" | "light" | "dark";

const STORAGE_KEY = "kea-theme";
const SETTING_KEY = "ui.theme";

interface ThemeContextValue {
  theme: Theme;
  preference: ThemePreference;
  setPreference: (preference: ThemePreference) => void;
  toggle: () => void;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

function getSystemTheme(): Theme {
  if (typeof window === "undefined") return "light";
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function isPreference(value: string | null): value is ThemePreference {
  return value === "system" || value === "light" || value === "dark";
}

function readLocalCache(): ThemePreference | null {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (isPreference(stored)) return stored;
  } catch {
    // localStorage unavailable
  }
  return null;
}

function writeLocalCache(preference: ThemePreference) {
  try {
    localStorage.setItem(STORAGE_KEY, preference);
  } catch {
    // localStorage unavailable
  }
}

function getInitialPreference(): ThemePreference {
  return readLocalCache() ?? "system";
}

function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme;
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [preference, setPreferenceState] = useState<ThemePreference>(getInitialPreference);
  const [systemTheme, setSystemTheme] = useState<Theme>(getSystemTheme);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const stored = await getSetting(SETTING_KEY);
        if (cancelled) return;

        // Backend empty/invalid: keep an existing user's cached preference
        // instead of clobbering it with "system".
        const cached = readLocalCache();
        const resolved = isPreference(stored) ? stored : cached ?? "system";
        setPreferenceState(resolved);
        writeLocalCache(resolved);
        if (!isPreference(stored) && cached) {
          // Push the cached preference to the backend to complete the migration.
          void setSetting(SETTING_KEY, cached).catch(() => {
            // invoke unavailable outside Tauri
          });
        }
      } catch {
        if (cancelled) return;
        setPreferenceState(readLocalCache() ?? "system");
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (preference !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => setSystemTheme(mq.matches ? "dark" : "light");
    onChange();
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [preference]);

  const theme: Theme = preference === "system" ? systemTheme : preference;

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  const setPreference = useCallback((next: ThemePreference) => {
    setPreferenceState(next);
    writeLocalCache(next);
    void setSetting(SETTING_KEY, next).catch(() => {
      // invoke unavailable outside Tauri
    });
  }, []);

  const toggle = useCallback(() => {
    setPreference(theme === "light" ? "dark" : "light");
  }, [theme, setPreference]);

  return (
    <ThemeContext.Provider value={{ theme, preference, setPreference, toggle }}>
      {children}
    </ThemeContext.Provider>
  );
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) {
    throw new Error("useTheme must be used within a ThemeProvider");
  }
  return ctx;
}
