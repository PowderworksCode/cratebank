import { mkdir, readFile, writeFile } from "node:fs/promises";
import { marked } from "marked";

const here = new URL("./", import.meta.url);
const [markdown, styles, template] = await Promise.all([
  readFile(new URL("site.md", here), "utf8"),
  readFile(new URL("site.css", here), "utf8"),
  readFile(new URL("site.template.html", here), "utf8"),
]);

const content = await marked.parse(markdown);
const page = template
  .replace("{{styles}}", () => styles)
  .replace("{{content}}", () => content);

if (page.includes("{{styles}}") || page.includes("{{content}}")) {
  throw new Error("site template has an unfilled placeholder");
}

await mkdir(new URL("dist/site/", here), { recursive: true });
await writeFile(new URL("dist/site/index.html", here), page);
