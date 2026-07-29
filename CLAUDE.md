## 작업을 시작할 때

- 먼저 `README.md`, `STATUS.md`와 현재 변경 사항을 확인한다.
- 아키텍처는 `docs/architecture.md`, DSP 수식은 `docs/dsp.md`, 실측 절차는
  `docs/measurement-protocol.md`, 검증 기준은 `docs/validation.md`를 참고한다.
- 이미 구현된 함수와 테스트를 검색하고 같은 기능을 중복 작성하지 않는다.

## 저장소의 주요 위치

- `crates/dsp-core`: UI나 장치에 의존하지 않는 순수 DSP
- `crates/audio-io`: 오디오 장치 검색과 마이크 캡처
- `crates/export`: WAV와 Roon ZIP 생성·검사
- `crates/dsp-cli`: fixture와 회귀 테스트 실행
- `apps/desktop/src`: React UI와 한국어/영어 문자열
- `apps/desktop/src-tauri`: Tauri 명령과 실제 측정 세션
- `assets/`: 저장소에 포함된 제품 자산(스윕 WAV, 예시 타깃 커브)
- `testdata/`: 테스트가 필요로 하는 작은 fixture

저장소에 올라가지 않는 개발자 로컬 디렉토리(.gitignore):

- `debugfiles/`: 디버깅에 필요한 파일을 개발자가 넣어주는 디렉토리
- `measurments/`, `examples/`: 개발자 개인의 실측 데이터와 생성 결과물
- `HANDOFF.md`, `DEVELOPERINFO.md`: 개발 과정 기록

로컬 fixture가 없으면 해당 테스트는 `SKIPPED:`를 출력하고 통과한다. 즉 새로 클론한
환경의 `cargo test`는 로컬보다 적은 범위를 검증한다.

## 이 프로젝트에서 중요한 안전 규칙

- predicted, synthetic 결과를 실제 측정 검증으로 표시하지 않는다.
- 실제 필터 적용 후 재측정 없이는 “보정 완료”라고 표시하지 않는다.
- cut-only, 보호된 딥, 감쇠 한계와 minimum-phase 설계 경로를 보존한다.
- 실제 마이크/Roon 테스트를 실행하지 않았다면 통과했다고 보고하지 않는다.
- UI 문구를 바꾸면 한국어와 영어를 함께 확인한다.

## 수정 후 기본 확인

- 일반 변경: 관련 단위 테스트를 먼저 실행한다.
- UI 변경: `npm test --prefix apps/desktop`
- Rust/DSP 변경: `cargo test --workspace`
- 최종 인계 전 필요하면 포맷, Clippy와 프로덕션 빌드를 실행한다.

## 답변 전 실행
컨텍스트가 길어질 경우 사용자가 새 채팅으로 이동할 것을 고려하여 HANDOFF.md 업데이트. 컨텍스트를 70~80%정도 사용 시 HANDOFF.md 업데이트 필수.

## 마지막 보고

- 완료한 결과
- 실행한 검증
- 실행하지 못한 수동 검증
- 남은 위험
- 다음 한 단계

## 기타 규칙

- 회귀 테스트는 매 수정마다 진행하지 말고 마지막에 한 번 진행할 것.
