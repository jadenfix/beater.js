import { Suspense } from "react";

async function DelayedFact() {
  await new Promise((resolve) => setTimeout(resolve, 280));
  return <span data-rsc-delayed>delayed server fact streamed after the shell</span>;
}

export default function HomeFlight() {
  return (
    <section data-rsc-panel>
      <strong data-rsc-shell>server component flight</strong>
      <span data-rsc-copy>rendered in the beater isolate and delivered as flight frames - cafe Δ</span>
      <Suspense fallback={<span data-rsc-fallback>loading server fact</span>}>
        <DelayedFact />
      </Suspense>
    </section>
  );
}
