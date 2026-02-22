import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./en.json";
import de from "./de.json";
import it from "./it.json";
import es from "./es.json";
import pt from "./pt.json";
import fr from "./fr.json";
import hr from "./hr.json";
import nl from "./nl.json";

const resources = {
  en: { translation: en },
  de: { translation: de },
  it: { translation: it },
  es: { translation: es },
  pt: { translation: pt },
  fr: { translation: fr },
  hr: { translation: hr },
  nl: { translation: nl },
};

i18n.use(initReactI18next).init({
  resources,
  lng: "en",
  fallbackLng: "en",
  interpolation: {
    escapeValue: false,
  },
});

export default i18n;
