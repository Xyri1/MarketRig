import "./style.css";
import { createApp } from "vue";
import { init } from "ghostty-web";
import App from "./App.vue";
import i18n from "./i18n";

await init();
createApp(App).use(i18n).mount("#app");
