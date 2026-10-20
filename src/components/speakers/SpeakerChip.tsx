import React, { useState } from "react";
import { useTranslation } from "react-i18next";

interface SpeakerChipProps {
  label: string;
  className: string;
  /**
   * Called with the new name (empty to go back to the default label). The
   * chip is only editable when given.
   */
  onRename?: (name: string) => void;
  /** Name shown in the editor: the custom one, or empty. */
  name?: string;
}

/** A segment's speaker; a click renames a person found by speaker detection. */
export const SpeakerChip: React.FC<SpeakerChipProps> = ({
  label,
  className,
  onRename,
  name = "",
}) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(name);
  const base = `text-xs font-medium px-1.5 py-0.5 rounded shrink-0 ${className}`;

  if (!onRename) {
    return <span className={base}>{label}</span>;
  }

  if (editing) {
    const commit = () => {
      setEditing(false);
      const next = value.trim();
      if (next !== name) onRename(next);
    };
    return (
      <input
        autoFocus
        value={value}
        placeholder={label}
        onChange={(e) => setValue(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") {
            setValue(name);
            setEditing(false);
          }
        }}
        aria-label={t("speakers.nameLabel")}
        className={`${base} w-32 bg-transparent border border-logo-primary focus:outline-none`}
      />
    );
  }

  return (
    <button
      type="button"
      onClick={() => {
        setValue(name);
        setEditing(true);
      }}
      title={t("speakers.rename", { name: label })}
      aria-label={t("speakers.rename", { name: label })}
      className={`${base} cursor-text hover:ring-1 hover:ring-current focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary`}
    >
      {label}
    </button>
  );
};
