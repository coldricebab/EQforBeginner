*[English](architecture.md) · 한국어*

# 아키텍처

주요 의존성/출처 참고 자료는 2026-07-24에 확인했고, 라이브 구현
경계는 2026-07-26에 갱신했다. 이 문서는 제품 결정을 기록하며,
`STATUS_ko.md`는 오늘 시점에 어떤 경계가 구현되어 있는지 기록한다.

## 결정 요약

| 항목 | 결정 | 고정/현재 버전 | 이유 |
| --- | --- | --- | --- |
| 데스크톱 셸 | Tauri 2 | Rust 크레이트 2.11.5, CLI 2.11.4 | 네이티브 시스템 웹뷰, 작은 번들 크기, Rust 명령 경계, Windows/macOS 지원 |
| UI | React + TypeScript + Vite | 정확한 버전은 `apps/desktop/package-lock.json` 참조 | 접근성 있는 컴포넌트 상태와, 무거운 차트 런타임 없이 이식 가능한 SVG 응답 플롯 |
| DSP | 순수 Rust 워크스페이스 라이브러리 | `eqforbeginner-dsp-core` 0.1.0 | 결정론적이고 오프라인이며 UI나 오디오 하드웨어 없이 테스트 가능 |
| 테스트 하네스 | Rust CLI | `eqforbeginner-cli` 0.1.0 | 반복 가능한 fixture, 리포트, WAV, 내보내기 검사를 생성 |
| 오디오 I/O (Phase 2) | CPAL | 0.18.1, MSRV 1.85 | 호스트 장치 ID/구성 탐색과, 명시적 입력 채널 추출을 갖춘 유계 네이티브 48 kHz PCM 캡처, CoreAudio 및 WASAPI 기본값; Apache-2.0 |
| 고급 무선 측정 전송 | 사용자가 Roon에서 직접 시작하는 로컬 WAV 재생; 마이크 측 인식/디컨볼루션 | `wireless-sweep-recognition-v1` + `known-sweep-deconvolution-v5` | 안전하지 않은 원격 전송/음량 변경을 피한다. 임의의 네트워크 지연은 탐색되는 캡처 오프셋이 되고, 고정된 부호 있는 사전 제로 윈도가 음향 마커 기준보다 먼저 도달하는 경로를 보존한다 |
| 마이크 캘리브레이션 | 엄격한 로컬 UMIK 형식 TXT | `umik-calibration-parser-v2` | 인용된 miniDSP 메타데이터를 포함해, 네트워크나 파일명 가정 없이 감사 가능한 로그 주파수/선형 dB 보간 |
| 라이브 세션 어댑터 | Tauri가 소유하는 불변 로컬 증거 | `similarrew-live-project-v5` (이름 변경 이후에도 유지되는 과거 디스크 포맷 id) | 장치/상태/파일을 순수 DSP 코어 바깥에 두면서, 모든 캡처를 2.0/2.1 선언, 분리 경로 크로스오버 계획, 예측 기반 단일 서브우퍼 순위, 확인된 수동 하드웨어 설정, 스윕/캘리브레이션 해시, 선택된 네이티브 입력 채널에 결속한다. 업로드된 WAV 마커 라우팅, 자동 완료, 스윕 단위 증거를 기록한다 |
| 기존 REW 데이터 브리지 | 개발 전용 버전드 JSON 변환 | 원본 REW 5.31.3; 선호는 REW 5.40+ 로컬 API | 비공개 `.mdat` 직렬화와 Python을 제품 런타임 밖에 유지한다 |
| 기준 샘플레이트 | 48 kHz | 제품 기본값 | 오프라인 시험 및 향후 최초 폐루프 검증 형식과 일치 |
| EQforBeginner Roon 후보 포맷 | ZIP 안의 인터리브 스테레오 IEEE float32 WAV | WAV 메타데이터가 레이아웃/레이트를 선택 | 직접 스테레오는 `.cfg`가 필요 없다. IEEE-float 수용 여부는 여전히 실제 Roon 스모크 테스트 게이트로 남는다 |

Tauri의 구성 요소는 각각 독립적인 릴리스 번호를 가진다. Rust 크레이트,
CLI, JavaScript API가 같은 번호를 공유하도록 강제하지 않는다. 재현 가능한
진실의 원천은 락파일이다.

출처:

- [Tauri release index](https://v2.tauri.app/release/)
- [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)
- [Tauri project structure](https://v2.tauri.app/start/project-structure/)
- [Tauri application icons](https://v2.tauri.app/develop/icons/)
- [Tauri distribution overview](https://v2.tauri.app/distribute/)
- [Tauri macOS native `Info.plist` configuration](https://v2.tauri.app/distribute/macos-application-bundle/)
- [Apple `NSMicrophoneUsageDescription`](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSMicrophoneUsageDescription)
- [Microsoft Windows microphone permissions](https://support.microsoft.com/en-us/windows/privacy/turn-on-app-permissions-for-your-microphone-in-windows)
- [CPAL 0.18.1 API and platform notes](https://docs.rs/cpal/0.18.1/cpal/)
- [CPAL DeviceTrait IDs and configurations](https://docs.rs/cpal/0.18.1/cpal/traits/trait.DeviceTrait.html)
- [REW local HTTP API](https://www.roomeqwizard.com/help/help_en-GB/html/api.html)
- [REW beta release history](https://www.roomeqwizard.com/beta.html)
- [Roon MUSE Convolution](https://help.roonlabs.com/portal/en/kb/articles/dsp-engine-convolution)
- [Roon Headroom Management](https://help.roonlabs.com/portal/en/kb/articles/dsp-engine-headroom-management)
- [Roon-supported audio formats](https://help.roonlabs.com/portal/en/kb/articles/faq-what-audio-file-formats-does-roon-support)
- [Adding local music to Roon](https://help.roonlabs.com/portal/en/kb/articles/adding-local-music-to-roon)
- [Roon Volume Leveling](https://help.roonlabs.com/portal/en/kb/articles/volume-leveling)
- [Roon Signal Path](https://help.roonlabs.com/portal/en/kb/articles/signal-path)
- [Roon Extension API](https://github.com/RoonLabs/node-roon-api)
- [Gamper 2017 asynchronous impulse-response measurement](https://www.microsoft.com/en-us/research/wp-content/uploads/2017/03/Clock_drift_estimation_HSCMA_2017.pdf)
- [Microsoft asynchronous IR reference implementation (MIT)](https://github.com/microsoft/Asynchronous_impulse_response_measurement)
- [ITU-R BS.1116-3 listening-room and stereo-reference conditions](https://www.itu.int/dms_pubrec/itu-r/rec/bs/R-REC-BS.1116-3-201502-I%21%21PDF-E.pdf)

## 프로세스 경계

```text
React wizard
  -> narrow typed Tauri commands
    -> project/session orchestration (offline diagnostics + live developer beta)
      -> audio_io          (discovery + selected-channel mono capture; no mock/fallback)
      -> measurement       (known-WAV recognition + calibrated IR/FR extraction)
      -> calibration       (strict UMIK TXT parser/interpolator)
      -> dsp-core          (Phase 1, Phase 3 ranking, Phase 4 replay implemented)
          analysis -> spatial -> target -> correction -> FIR -> validation
          sub_integration -> measured candidate ranking + separated-path check
          phase4 -> measured-response design + predicted-only numerical gates
          phase6 -> six native grids + cross-rate response validation
      -> live adapter      (evidence admission, persistence, closed-loop gate, headroom)
      -> export            (trial/final WAV/ZIP writer + strict package readback)
      -> download adapter  (current-artifact selection + native save dialog + byte check)
```

DSP 라이브러리는 윈도우, 장치 핸들, 전역 상태, 네트워크 클라이언트,
벽시계 기반 결정을 절대 소유하지 않는다. 동일한 배열과 설정은 반드시 동일한
결과를 내야 한다. UI는 원시 FFT 빈을 절대 스스로 해석하지 않는다. 라이브
Tauri 세션 서비스가 애플리케이션 데이터 디렉터리, 해시로 식별되는
캘리브레이션/스윕 입력, 덮어쓰기 없는 원시 캡처 WAV, 측정 스냅샷,
시험/최종 산출물, 상태 무효화를 소유한다.
이는 완전한 재열기/마이그레이션 UI가 아니라 개발자 베타 수준의 영속화다.
앱을 재시작해도 이전 라이브 세션은 다시 열리지 않는다. 다만 마법사는,
시스템 모드, 장치/채널, 캘리브레이션 및 스윕 해시, 수동 서브우퍼 상태,
DSP 버전이 모두 일치하는 이전 프로젝트들에서 각 측정 종류/측정 위치/채널
키별로 가장 최근에 수락된 캡처를 명시적으로 복원할 수 있다. 프로젝트는
최신순으로 스캔되므로, 더 오래된 프로젝트는 더 새로운 호환 프로젝트에
없는 키만 채운다.

## Rust 모듈 경계와 현재 상태

- `audio_io`: 호스트로 한정된 장치 ID/메타데이터와 지원되는 48 kHz 구성
  탐색이 CPAL로 구현되어 있다. 선택된 정확한 ID는, 선택된 입력 채널을 포함하는
  네이티브 48 kHz PCM 구성을 노출할 때 열 수 있다.
  인터리브 콜백은 프레임 수가 계수되고, 해당 채널만 취소 기능, 피크/클립,
  xrun, 콜백 락 손실, 타임스탬프 간극, 누락된 꼬리, 런타임 오류 증거를 갖춘
  명시적으로 유계인 모노 버퍼로 추출된다. 콜백은 락 중첩 시 완전한 PCM 블록을
  조용히 버리는 대신 모니터의 유계 꼬리 memcpy를 기다린다. 타임스탬프
  간극/역행은 진단 정보로 보존되지만 그 자체를 PCM 손실이라고 부르지는 않는다.
  xrun, 누락 프레임, 콜백 손실, 포맷 오류, 런타임 스트림 오류는 여전히
  하드 실패다. 호출자는 데이터 기반 완료 감지기를 제공할 수 있다.
  이 감지기는 유계로 겹치는 꼬리 스냅샷과 그 절대 오프셋만 받으므로, 계속
  늘어나는 전체 캡처를 복제하지 않고도 마커 상관이 콜백 락 바깥에서 실행된다.
  다운믹스, 폴백, 가짜 provider는 존재하지 않는다. 출력 재생, 듀플렉스 동기화,
  임의의 라우팅 또는 믹싱 맵은 여전히 미구현이다.
- `measurement` (부분 구현): 가져온 48 kHz WAV를 디코딩하고 SHA-256으로
  식별한 뒤, `wireless-sweep-recognition-v1`이 블록 FFT 제로평균 상관과
  독립 세그먼트 검사로 실제 모노 캡처를 탐색한다. `known-sweep-deconvolution-v5`는
  자격을 갖춘 반복 마커 증거가 있을 때만 인식된 세그먼트를 리샘플링하고,
  정규화된 스펙트럼 나눗셈을 수행하며, 마이크 크기 캘리브레이션을 적용하고,
  32,768 샘플 원시/캘리브레이션 임펄스 응답 쌍과 65,536 포인트(0.73 Hz)
  분석 주파수 응답을 보존하며, SNR/재구성/클립/타이밍 품질을 보고한다.
  마커를 기준으로 하는 캡처는 피크 정렬 대신 고정된 4,800 샘플 사전 제로
  윈도를 사용하여, 오른쪽 스피커 타이밍 마커에 대한 메인/서브우퍼 도달
  시간의 부호 있는 차이를 보존한다. 두 마커가 모두 독립적인 구간 및 클록
  증거를 제공하면, 파형 재구성 적합도는 기록되지만 수용 게이트는 아니다.
  마커 없는 폴백은 12 dB 게이트를 유지한다.
  업로드 분석은 메인 영역과 두 마커 영역을 분리하고, 각 마커에서 L/R 에너지와
  상관을 측정하며, 지배적인 재생 채널을 보고하고 그 채널의 파형을 인식
  템플릿으로 사용한다. 라이브 엔드포인트는 앞쪽 마커를 인식하고, 메인 스윕을
  계측하며, 길이가 같지 않은 반복 마커 후보를 보존하고, 그 쌍 간격을 검증하며,
  고정된 추가 지연 없이 완전한 뒤쪽 마커에서 종료한다. 전체 스윕 인식이 폴백이다.
  스윕 생성, 인증된 절대 SPL, 음향 채널 스왑 감지, 견고한
  트랜지언트/마이크 이동 분류는 여전히 미구현이다. UMIK Sens
  Factor는 명시적으로 가정된 SPL 추정치를 제공할 수 있으며, dBFS/클리핑은
  여전히 fail-closed 게이트로 남는다.
- `calibration`: `umik-calibration-parser-v2`는 2열/3열 UMIK 형식 텍스트를
  파싱하고, 시리얼/감도/위상 출처 정보를 보존하며, 20 Hz~20 kHz 커버리지를
  검증하고, 로그 주파수 상에서 dB 단위로 크기 보정을 선형 보간한다. V1은
  선택적 위상 열을 적용하지 않는다. Sens Factor는 임펄스 응답/주파수 응답을
  바꾸지 않으며, 가정임이 표시된 레벨 추정치에만 반영된다.
- `analysis`: FFT 응답, 위상/그룹 지연 산출물과 스무딩.
- `spatial`: 보존된 측정 위치 전반의 가중 에너지 평균과 견고한 산포.
- `target`: 버전드 스타일 프리셋과 사용자 텍스트 타깃 파싱.
- `correction`: 보호된 딥과 공간적으로 견고한 유계 크기 요청. 여러 측정 위치에서
  반복되는 넓고 얕은 결손만 최대 +3 dB를 받을 수 있다.
- `fir`: minimum-phase 스펙트럼 인수분해와 인과적 FIR 추출.
- `stereo`: 매끄러운 채널 정책 전이를 갖춘 공통 저역 보정.
- `validation`: 수치 불변식과 응답 영역 예측 지표.
- `phase4`: 48 kHz 실측 응답 재생, 안전한 재설계, minimum-phase FIR
  합성, 보호된 딥 검사, 타임라인을 보존하는 임펄스 응답 컨볼루션 진단.
  이 결과 타입은 `predicted-only-measured`만 표현할 수 있으며, 예측을 하드웨어
  검증으로 승격시키는 호출자 불리언을 절대 받아들일 수 없다. Tauri 라이브
  어댑터는 별도로 실제 수락된 응답 집합을 동일한 함수에 공급하고, 시험 이후의
  폐루프 증거 상태를 소유한다.
- `phase6`: 하나의 물리 주파수 기반 유계 게인 의도를 여섯 개 Roon 레이트
  전부에서 재설계하고, 감쇠 전용 안전 정규화를 정렬하며, 실현된 크기와
  상대 그룹 지연을 내보내기 자격과 독립적으로 검증한다.
- `sub_integration`: 순수 Rust 실측 후보 모델, 선언된 설정에
  크로스오버/지연/극성/레벨이 포함될 수 있는 후보에 대한 결정론적 응답 점수,
  각 크로스오버에서 물리적으로 측정된 메인 전용/서브우퍼 전용 경로로부터
  메인 지연 및 0/180도 극성 대안을 유계 복소 합성하는 기능,
  증거 누락 처리, 분리된 메인/서브우퍼 복소합 진단이
  구현되어 있다. 측정되지 않은 설정은 생성하지 않는다. 하드웨어 능력
  수집, 한 번에 한 가지만 바꾸는 안내형 캡처, 다중 측정 위치 취득,
  최종 확인 측정은 여전히 미구현이다.
- `export`: WAV/패키지 생성과 구조 검증.
- `project` (부분 구현): CLI/fixture 진단이 버전드 파생 JSON을 영속화한다.
  `live_measurement.rs`는 이제 불변 입력, 원시 캡처, 측정별
  스냅샷, 커스텀 타깃 TXT/해시/파서/범위 메타데이터, 선택된 타깃 식별자,
  최종 증거, 산출물 해시를 플랫폼 애플리케이션 데이터 디렉터리 아래에 영속화한다.
  `accepted-measurement-snapshot-cache-v3`은 사용자의 명시적 요청 시 모든
  측정 키에 대해 독립적으로 가장 최근의 호환 수락 스냅샷을 복원할 수 있으며,
  재시도 실패는 해당 측정 키의 마지막 수락 값을 절대 밀어내지 못한다. 전체
  세션 재열기/마이그레이션, 사용자에게 노출되는 제외 이력 편집기, 정리/내보내기
  관리는 구현되어 있지 않다.

### 라이브 개발자 베타의 재사용 경계

라이브 경로에는 의도적으로 두 번째 보정 구현이 존재하지 않는다.

- 재사용되는 보정 및 내보내기 파이프라인: CPAL 캡처와 텔레메트리, 무선
  인식기, Phase 4 에너지 통계/타깃/유계 보호된 딥 목적함수/스테레오
  블렌드/minimum-phase FIR/예측, Phase 6 네이티브 샘플레이트 합성/교차 레이트
  게이트, WAV/ZIP 생성/재판독, 그리고 현재 세션의 시험 ZIP 또는 마지막으로
  검증된 최종 ZIP만 허용하는 네이티브 저장 대화상자 다운로드 경계.
- 새로 작성된 증거/적응 코드: UMIK 파싱/보간, known-sweep
  디컨볼루션/품질, 선택적 전/후 마커 클록 매핑, 불변 세션
  영속화, 수락된 L/R 쌍을 Phase 4 응답 모델로 변환, 마이크/증거 생성
  잠금, 유계 완전 세그먼트 마커 수용에 이은 엄격한 두 마커 간격 검증,
  손으로 재배치한 P0 종료 반복의 개략 안정성 검사, 영속화된 수동
  시험 활성화 확인 진술, 시험 이후 P0 폐루프 비교, 검증된 시험과
  네이티브 48k 응답의 결속, 보수적인 신호/FIR 상한 헤드룸,
  수용된 Raw/Target/Predicted/Verified 배열을 표시 전용 1/12 옥타브
  스무딩과 함께 유계 로그 그리드에 직렬화하는 `measured-fr-result-plot-v2`,
  그리고 동적 6단계 스테레오/7단계 2.1 라이브 GUI. 이전의 전역
  고급 설정 페이지는 마운트되지 않는다.

새 코드는 실제 증거가 기존 DSP에 어떻게 들어오고 무엇을 인가하는지를 바꿀 뿐,
MDAT에서 파생된 fixture로 이미 검증해 온 알고리즘을 대체하지 않는다. 유일한
설계 그리드 적응은 명시적이다. 라이브 Phase 4 시험은 기존 Phase 6의
48 kHz 네이티브 길이인 16,320 샘플을 사용하므로, 선언하고 재측정한 시험을
최종 패키지에 응답 기준으로 결속할 수 있다. 오프라인 Phase 4 기본값은 16,384로 유지된다.

## 신호와 타이밍 모델

샘플 데이터는 장치 경계에서 정규화된 `f32`를, 오프라인
분석/설계에서는 `f64`를 사용한다. 측정은 원래의 샘플 타임라인을 유지한다. 음향
도달 시간, 선택적 고정 채널 지연, 주파수 의존 그룹 지연,
FIR 지연은 서로 구분되는 필드다. Phase 1 합성 데이터는 선언된 임펄스
원점을 가지며, 실제 음향 타이밍을 검증한다고 주장하지 않는다.

라이브 데스크톱 명령은 `scan_default_host_inputs`를 사용한다. CPAL에 입력
장치와 그 구성만 요청한다. 기본 출력을 조회하거나, 출력 장치를 열거하거나,
출력 구성을 검사하거나, 출력 스트림을 열지 않는다. 입력
탐색은 호스트로 한정된 ID, 호스트 API, 장치 메타데이터, 채널 수,
샘플 포맷, 버퍼 범위, 지원되는 프로젝트 샘플레이트를 명시적으로 노출한다.
CPAL은 백엔드가 허용하는 범위에서 장치 ID를 영속화 가능하다고 설명한다. 따라서
저장된 ID는 힌트일 뿐이며, 스트림을 열기 전에 현재 메타데이터와 대조해 재검증해야 한다.
macOS의 기본값은 CoreAudio, Windows의 기본값은 WASAPI다. CPAL의 ASIO 기능은
SDK/툴체인과 라이선스/배포 검토를 추가로 요구하므로 ASIO는 옵트인으로 남는다.

CPAL은 입력과 출력을 별개의 스트림으로 노출하며, 재생 장치와 USB 마이크의
동기화된 클록을 문서화하지 않는다. 따라서 콜백 타임스탬프만으로는 불충분하다는
것이 명시적인 제품 차원의 추론이다. 측정 계층은
자신의 음향 마커를 보존하고 녹음된 타임라인 위에서 드리프트를 추정해야 한다.

고급 무선 경로와 완전한 라이브 경로는 의도적으로 어떤 오디오도 출력하지 않는다.
사용자는 동일한 WAV를 EQforBeginner와 Roon에 각각 가져오고, 마이크를 명시적으로
준비시킨 뒤, Roon에서 재생을 시작한다.
앱이 캡처를 탐색하므로, Roon/RAAT 버퍼링 지연은 스피커 도달 시간으로 오인되지
않고 보고되는 오프셋의 일부가 된다. Roon에는 공식 Extension API가 있지만,
이 베타는 그것과 페어링하지 않고 트랜스포트, Zone, 대기열, 음량을 제어하지 않는다.
EQforBeginner는 물리적 앰프 레벨을 알 수 없고, 자동 스윕 재생은
피할 수 있는 안전 위험을 만들기 때문이다.

하나의 스윕에서 주파수가 변하는 세그먼트들은 유효 샘플레이트 기울기만 제공한다.
실내의 주파수 의존 지연이 그 적합을 편향시킬 수 있으므로, 결과는 항상
`intra_sweep_segment_fit`으로 저장된다. 제공되는 스테레오 스윕 파일에는 추가로
에너지가 낮은 전/후 마커 이벤트가 들어 있다. 둘 다 인식되면 라이브 어댑터는 그
동등한 이벤트들로부터

```text
capture_sample = offset + clock_ratio * reference_sample
```

를 적합시키고, 그 비율을 디컨볼루션 클록 매핑에 사용한다. 마커 쌍을 확립할 수
없으면, 어댑터는 실내에 편향된 스윕 내부 기울기에서 워프 크기를 가져오는 대신
의도적으로 비율 1.0을 사용한다. 어느 경우에도 동일 측정 위치의 L/R 도달
재현성은 확립할 수 없다. 모든 라이브 캡처는
`timing_eligible=false`로 남고, 어떤 피크도 0으로 이동되지 않으며, L/R 지연 보정은
비활성 상태로 유지된다. 이는 CPAL 콜백 시각이나 단일 임펄스 최댓값을 음향
타이밍 기준으로 취급하지 않으면서 위의 비동기 측정 접근법을 따른다.

## 기존 REW 측정 경계

애플리케이션과 Rust DSP 라이브러리는 `.mdat`를 파싱하지 않는다. 실측 Phase 3와
Phase 4 개발용 fixture는 격리된 Python 환경에서 고정된
`javaobj-py3` 0.5.0 헬퍼를 사용하는 개발 전용 변환기로 생성했다. 변환기는
측정된 SPL과 위상을 보존하고, 타임라인을 옮기거나 레벨을 정렬하지 않으며, 원본
메타데이터를 기록하고, 모든 원본에 대해 SHA-256과 바이트 크기를 저장한다. 또한
REW의 0이 아닌 선형 그리드 원점을 보존한다. 저장된 첫 응답 샘플은 임의로 지어낸
0 Hz 원점이 아니라 기록된 `startFreq`와 연결된다. Rust CLI는
버전드 JSON fixture를 소비하며 분석 전에 원본 파일을 검증한다. Python과
비공개 Java 직렬화 리더는 데스크톱 런타임 또는 배포 의존성이 아니다.

이 변환된 fixture와 그 `.mdat` 원본은 개발자 한 명의 개인 룸 측정치다.
용량이 수백 메가바이트에 이르고, 공개 저장소의 일부가 아니며,
애플리케이션을 빌드/실행/테스트하는 데 필요하지 않다. 이를 소비하는 회귀 테스트는
해당 디렉터리가 없으면 `SKIPPED:` 줄을 출력한다.

선호되는 유지보수 가능한 브리지는 REW 5.40 이상의 문서화된 localhost HTTP
API다. 공식 문서는 `-api`로의 시작, 선택적 `-nogui`, 측정
로드, 그리고 `127.0.0.1`(기본 포트 4735)의 주파수 응답/위상 및 임펄스 응답
엔드포인트를 노출한다. 이 파일들에 사용한 설치본 REW 5.31.3은 `-api`로
시작했을 때 API가 지원되지 않는다고 보고했으므로, 추출 경로로 사용하지 않았다.
2026-07-19 기준으로 공식 베타 이력에는 2026-07-12자 REW
5.40 beta 130이 올라와 있다. 향후 개발은 비공개 `.mdat` 리더를 확장하기보다
그 문서화된 API를 선호해야 하며, 최종 앱은 자신의 측정에 대해 REW로부터 독립적으로 남는다.

## Phase 4 오프라인 응답 재생 경계

`phase4-response-replay-v2`는 신뢰된 여섯 개의 48 kHz XO90 소스를 받는다. 실측 L+서브우퍼와
R+서브우퍼 기준선, L/R 메인 전용, 서브우퍼 전용 A/B다. (이들은 공개 저장소의 일부가 아닌
개발자 로컬 `measurments/phase4` 디렉터리에 있다.) 필터 설계에 쓰이는 응답은 두 개의
결합 응답뿐이다. 분리 경로는 수용 진단일 뿐, 결합 재생 경로 데이터의 대체물이 아니다.

REW가 저장한 캘리브레이션된 크기 응답이 권위 있는 설계 입력이다. 원시 REW 임펄스
응답의 단순 FFT는 REW의 응답 윈도와 캘리브레이션 의미를 재현하지 못하므로, 원시
L/R 결합 임펄스 응답은 크기 재설계에 사용하지 않는다. 이들은 각자의 원래 `startTime`을
보존한 채 유한한 시간 영역 거동을 확인하기 위해서만 후보 FIR과 컨볼루션된다.
이들의 샘플 최댓값은 클리핑 증거도, 재생 트루 피크 증거도 아니다.

결정론적 오프라인 경로는 네이티브 48 kHz 그리드 위에서 16,384탭 스테레오
minimum-phase FIR을 설계한다. 20~500 Hz를 보정하고, 650 Hz까지 단위 이득으로
복귀하며, 공간적으로 뒷받침되는 넓은 딥의 부스트를 +3 dB로 제한하고, 최초의
컷 요청이 감쇠 한계를 초과하면 타입이 지정된 안전한 재설계를 적용하며, 수치
예측 산출물을 발행한다. 수용 조건은 레벨/타임라인 메타데이터가 변하지 않았을 것,
원본 해시, 호환되는 UMIK 캘리브레이션 및 타이밍
메타데이터, 그리고 45~180 Hz 분리 경로 복소합 검사 네 가지 모두가 1.0 dB
RMSE 이하일 것이다.

이 경계는 의도적으로 단방향이다. 유일한 검증 상태는
`predicted-only-measured`다. 이 fixture에는 FIR 적용 후의 재생 경로 캡처,
동일 설정 결합 반복 측정, 다중 측정 위치 확인이 존재하지 않는다. 따라서
`hardware_verification=unverified`, `closed_loop_passed=null`, `export_eligible=false`,
`recommended_headroom_db=null`이다. 이 오프라인 경로는 시험 WAV와 리포트를 내보내며,
Roon ZIP은 절대 만들지 않는다. 별도로 구현된 라이브 세션 경로는 누락된
시험 이후 P0 증거를 수집할 수 있지만, 이 fixture 프로젝트를 변형하거나 승격시키지 않는다.
Phase 4 프로젝트 스키마 v2는 시험 WAV의 SHA-256과 바이트 크기를 기록하여, 하류
소비자가 다른 스테레오 파일을 조용히 바꿔치기할 수 없게 한다.

## Phase 6 네이티브 샘플레이트 및 증거 경계

`phase6-native-six-rate-v2`는 48 kHz 임펄스의 리샘플 복사본이 아니라 Phase 4의
물리 주파수 기반 유계 설계를 소비한다. 재설계 전에 CLI는 그 설계를
기존 Phase 4의 48 kHz/16,384 샘플 그리드에서 재합성하고, 해시로 연결된 원본 WAV와
탭 단위 float32가 `1e-6` 이내로 일치할 것을 요구한다(현재 잔차는
`3e-11` 미만이다). 이는 새 네이티브 그리드가 동일한 샘플을 가져야 한다고 취급하지 않으면서
`filter-design.csv`를 이전 산출물에 결속한다.

수용된 의도는 이후 44.1, 48, 88.2, 96, 176.4,
192 kHz에서 각각 독립적으로 합성된다. 모든 네이티브 그리드는 정확히 340 ms의 길이를
가지므로, 각 레이트의 나이퀴스트 범위를 보존하면서 동일한
`50/17 Hz` 빈 간격을 갖는다. 짝수 필터
길이는 14,994, 16,320, 29,988, 32,640, 59,976, 65,280 샘플이다. RustFFT가
이 2의 거듭제곱이 아닌 길이를 처리한다. Phase 4는 1,024 이상의 짝수 길이를 받는다.
오프라인 기본값은 16,384로 유지되고, 라이브 경로는 네이티브 16,320 샘플 48 kHz 그리드를 선택한다.
공통 감쇠 전용 안전 정규화가 L/R과 모든 레이트에 걸쳐 적용되므로
어떤 레이트도 레벨이 올라가거나 레이트별 광대역 오프셋을 받지 않는다. 20 Hz~20 kHz의
조밀한 크기와 20~650 Hz의 상대 그룹 지연을 48 kHz
구현과 비교한다.

증거와 패키징은 분리되어 있다. 오프라인 실측 개발자 프리뷰는 여섯 개의
스테레오 float32 엔지니어링 WAV와 감사용 프로젝트를 쓰지만, 그 Phase 4 입력에는
FIR 적용 후 캡처도, 검증된 트루 피크도 없고 `export_eligible=false`이므로 Roon ZIP을
절대 만들 수 없다. 명백히 합성적인 기준 명령이 정확히 여섯 레이트를 담는 ZIP
작성기와 엄격한 재파싱 검증기를 실행한다. 이 오프라인 산출물들은
포맷/엔지니어링 증거일 뿐이다.

라이브 어댑터는 별개의 인가 경계를 가진다. 수락된 기준선
P0 L/R, 이미 수치적으로 통과한 시험, 수락된 시험 이후 P0 L/R, 기존의 모든
주파수 응답 검증 게이트, 그리고 20~650 Hz 구간에서 채널별로 1/12 옥타브 스무딩된
예측 대 검증 RMSE가 3 dB 이하일 것을 요구한다. 스무딩하지 않은 값은
진단용으로 보존되며 광대역 레벨 오프셋은 제거하지 않는다. 이 조건이 모두 충족될
때만 수용된 게인 의도를 동일한 Phase 6 함수에 전달하고,
여섯 개의 네이티브 WAV를 쓰고, 엄격한 ZIP을 만들어 재판독하며, 그 SHA-256을 영속화한다.
수동 "정확한 시험 활성" 선언은 타임스탬프가 찍힌 백엔드 증거일 뿐, Roon
제어나 증명이 아니다. `verified-trial-native48-response-binding-v1`은 최종
네이티브 48 kHz 응답이 검증된 시험과 크기 0.05 dB, 상대 그룹 지연
0.02 ms 이내로 일치할 것도 요구한다.

`validation-signal-and-response-peak-v3`은 등록된 L/R 스윕을 48 kHz 필터와
컨볼루션했을 때의 최악 4배 오버샘플링 출력/입력 피크 비와
스테레오 FIR L1 최악 사례 샘플 피크 상한을 모두 계산한다. 둘 중 큰 값을 취하고,
0 dB에서 하한을 두며, 1 dB를 더하고, 0.1 dB 단위로 올림한다. 이는 보수적인 Roon 시작
헤드룸이며, 임의의 프로그램 소스나 아날로그 재생 경로에 대한 보장이 아니다.

## Roon 패키징 계약

Roon은 하나 이상의 임펄스 응답 파일을 담은 ZIP을 공식적으로 받아들이며,
파일 메타데이터로부터 가장 가까운 채널 레이아웃과 샘플레이트를 선택한다. 정확한
레이트가 없으면 Roon이 리샘플링한다. 따라서 최종 패키지는 48 kHz
FIR을 리샘플링하는 대신 44.1, 48, 88.2, 96, 176.4, 192 kHz의 네이티브
설계를 담는다. 안정적인 이름은 `EQforBeginner_<rate>_stereo.wav`를 쓴다. 이 이름은
우리 관례이지 Roon의 요구 사항이 아니다.

제품 요구 사항은 ZIP 안에 `README.txt`도 포함할 것을 요구한다. Roon의 공개 페이지는
임의의 추가 파일이 어떻게 처리되는지 명시적으로 약속하지 않으며, 지원되는
32비트 WAV 사례가 IEEE float라고 따로 밝히지도 않는다. 따라서 이 두 세부 사항과
Mac/Windows용 Roon에서의 패키지 로딩은 여전히 실제 임포트 테스트 게이트로 남는다. 직접
스테레오 WAV 레이아웃에서는 `.cfg`를 생략한다.

오프라인 Phase 4 시험 WAV와 실측 프리뷰 WAV는 최종 패키지가 아니다. 라이브
시험 ZIP도 명시적으로 예측 전용이다. 정확한 시험 활성화 확인 진술 이후에 새로
수행한 P0 캡처, 응답 결속, 유계 헤드룸 계산을 거쳐 생성된 라이브 최종 ZIP만이
개발자 베타 청취용 산출물이다. 앱은 Roon 상태를
조회하거나 암호학적으로 검증하지 않으며, 실제 Roon 임포트/클리핑
스모크 테스트는 macOS와 Windows에서 여전히 필요하다.
시험 및 최종 마법사 카드는 네이티브 저장 대화상자 버튼을 노출한다. 복사 전에
백엔드는 라이브 세션 상태에서 산출물을 결정하고, 내부 프로젝트 루트 안으로
한정하며, 해당하는 단일 레이트 또는 정확히 여섯 레이트 ZIP 검증기를 다시 실행하고,
저장된 바이트를 확인한다. 프런트엔드는 임의의 내부 소스 경로를 지정할 수 없다.

원본 투명 번들 아트워크는
`apps/desktop/src-tauri/app-icon.png`에 보존되어 있다. Tauri가 생성한 PNG, `.icns`, `.ico` 자산은
저장소에 체크인되어 있으며 `tauri.conf.json`이 명시적으로 참조한다. 빌드 스크립트는
이들을 플레이스홀더로 대체하지 않는다. 이 베타에는 서명이나 공증
자격 증명이 포함되어 있지 않으며, Windows 설치 프로그램은 여전히 Windows 빌드/테스트 실행이 필요하다.

## 개발 사전 요구 사항

- macOS: Command Line Tools (`xcode-select --install`), Rust >= 1.85, Node LTS와 npm.
- Windows 11: Desktop C++ 워크로드를 포함한 Microsoft C++ Build Tools, WebView2,
  안정 MSVC Rust >= 1.85, Node LTS와 npm.

로컬 Phase 0 검증은 macOS 26.5.2, Apple clang 21.0.0, Homebrew Rust
1.97.0, Node 26.5.0, npm 11.17.0에서 수행했다. Windows 소스 검증은 Windows
러너를 사용할 수 있을 때까지 CI에 위임한다.

## 라이선스 정책

프로젝트 소스는 MIT다. 모든 런타임 의존성은 상용 데스크톱 배포와
호환되어야 한다(MIT, BSD, ISC, Zlib, Apache-2.0 선호). CPAL과 Tauri는
MIT/Apache 계열 라이선스다. 직접 사용하는 `base64` 0.22 fixture 디코딩과 `sha2`
0.10.9는 MIT/Apache-2.0이고, `hound` 3.5.1 WAV 디코딩은 Apache-2.0이다. Tauri
웹뷰 스택의 일부 전이 의존성은 MPL-2.0인데, 이는 파일 범위
카피레프트로 해당 파일에 대한 수정에만 의무를 부과하며, 여기서는 그 파일들을
수정하지 않는다. 강한 카피레프트(GPL/AGPL) 라이브러리와 독점 SDK는 별도의
배포 검토 없이 도입하지 않는다. 개발 전용 `.mdat`
변환기와 그 Python 환경은 애플리케이션 패키지에 포함되지 않는다. 향후 이들을
배포하기로 결정한다면 별도의 의존성 및 라이선스 검토가 필요하다.
