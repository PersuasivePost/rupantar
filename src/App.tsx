import { inTauri } from "./api";
import Playground from "./Playground";
import RadialHost from "./RadialHost";
import Settings from "./Settings";

// The desktop shell loads this same page in two windows, told apart by the URL hash.
// In a plain browser, `/#/settings` previews the settings page; anything else is the playground.
export default function App() {
  if (window.location.hash.startsWith("#/settings")) return <Settings />;
  return inTauri() ? <RadialHost /> : <Playground />;
}
