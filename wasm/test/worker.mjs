// The Worker used by the workerd test: runs a command script and returns
// the results with the files the module wrote.
import module from "./agent-browser.wasm";
import { createAgentBrowser } from "./host.mjs";
import skillsJson from "./skills.json";

const skills = JSON.parse(skillsJson);

export default {
  async fetch(request) {
    const { cdp, script, cwd } = await request.json();
    const browser = createAgentBrowser({ module, skills, env: { AGENT_BROWSER_CDP: cdp, NO_COLOR: "1" } });
    const results = [];
    for (const argv of script) results.push(await browser.run(argv, { cwd }));
    const files = browser.fs.drainChanges().map((path) => [path, browser.fs.readFile(path).length]);
    browser.dispose();
    return Response.json({ results, files });
  },
};
