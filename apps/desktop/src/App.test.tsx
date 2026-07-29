import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import App from "./App";

describe("App product surface", () => {
  it("mounts only the live wizard without the old global advanced page", () => {
    const html = renderToStaticMarkup(<App />);

    expect(html).toContain("실측 룸 보정 세션");
    expect(html).toContain("시스템 종류에 따른 실측 룸 보정 단계");
    expect(html).toContain("2채널 (L/R)");
    expect(html).toContain("2.1채널 (L/R + 단일 Sub)");
    expect(html).not.toContain("고급 설정");
    expect(html).not.toContain("Roon 무선 스윕 인식");
    expect(html).not.toContain("오프라인 fixture 파이프라인");
    expect(html).not.toContain("주파수응답 미리보기");
  });
});
