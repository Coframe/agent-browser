// The Worker used by the workerd tests. A request with `script` runs every
// command in that request. A request with `argv` runs one command in a
// Durable Object, which keeps the session between requests.
import module from "./agent-browser.wasm";
import { createAgentBrowser } from "./host.mjs";
import skillsJson from "./skills.json";

const skills = JSON.parse(skillsJson);

/**
 * Holds one agent-browser across requests. A plain Worker cannot: workerd
 * does not let a request wake work that another request started.
 */
export class BrowserSession {
  async fetch(request) {
    const { cdp, argv, cwd } = await request.json();
    this.browser ??= createAgentBrowser({ module, skills, env: { AGENT_BROWSER_CDP: cdp, NO_COLOR: "1" } });
    const result = await this.browser.run(argv, { cwd });
    const files = this.browser.fs.drainChanges().map((path) => [path, this.browser.fs.readFile(path).length]);
    return Response.json({ result, files });
  }
}

export default {
  async fetch(request, env) {
    const { cdp, script, cwd, argv } = await request.clone().json();
    if (argv) return env.BROWSER.get(env.BROWSER.idFromName("test")).fetch(request);
    const browser = createAgentBrowser({ module, skills, env: { AGENT_BROWSER_CDP: cdp, NO_COLOR: "1" } });
    const results = [];
    for (const argv of script) results.push(await browser.run(argv, { cwd }));
    const files = browser.fs.drainChanges().map((path) => [path, browser.fs.readFile(path).length]);
    browser.dispose();
    return Response.json({ results, files });
  },
};
