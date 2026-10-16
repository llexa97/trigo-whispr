import React from "react";
import { useSettings } from "../../hooks/useSettings";
import { GlobalShortcutInput } from "./GlobalShortcutInput";
import { TrigoWhisprKeysShortcutInput } from "./TrigoWhisprKeysShortcutInput";

interface ShortcutInputProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
  shortcutId: string;
  disabled?: boolean;
}

/**
 * Wrapper component that selects the appropriate shortcut input implementation
 * based on the keyboard_implementation setting.
 *
 * - "tauri" (default): Uses GlobalShortcutInput with JS keyboard events
 * - "trigo_whispr_keys": Uses TrigoWhisprKeysShortcutInput with backend key events
 */
export const ShortcutInput: React.FC<ShortcutInputProps> = (props) => {
  const { getSetting } = useSettings();
  const keyboardImplementation = getSetting("keyboard_implementation");

  // Default to Tauri implementation if not set
  if (keyboardImplementation === "trigo_whispr_keys") {
    return <TrigoWhisprKeysShortcutInput {...props} />;
  }

  return <GlobalShortcutInput {...props} />;
};
