import React from "react";
import { useTranslation } from "react-i18next";
import { LANGUAGES } from "@/lib/constants/languages";

interface LanguageOverrideProps {
  /** Forced language; `null` uses the language chosen in the settings. */
  value: string | null;
  onChange: (language: string | null) => void;
  id?: string;
}

/** Picks the language one file is transcribed in. */
export const LanguageOverride: React.FC<LanguageOverrideProps> = ({
  value,
  onChange,
  id,
}) => {
  const { t } = useTranslation();
  return (
    <select
      id={id}
      value={value ?? ""}
      onChange={(e) => onChange(e.target.value || null)}
      className="px-2 py-1 text-xs bg-mid-gray/10 border border-mid-gray/20 rounded-md focus:outline-none focus:border-logo-primary cursor-pointer"
    >
      <option value="">
        {t("transcriptions.options.languageFromSettings")}
      </option>
      {LANGUAGES.map((language) => (
        <option key={language.value} value={language.value}>
          {language.value === "auto"
            ? t("transcriptions.options.languageAuto")
            : language.label}
        </option>
      ))}
    </select>
  );
};
