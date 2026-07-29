*[English](ux-research.md) · 한국어*

# UX 리서치: 공개된 Dirac Live 워크플로

2026-07-18 검토. 이 리서치는 공개된 사용자 워크플로 자료만 사용하며, 독점 알고리즘,
UI 에셋, 내부 동작을 추론하거나 재현하지 않는다.

공개된 Dirac 안내는 일관되게 점진적 경로를 제시한다. 마이크 접근 권한 부여, 장치
검색/선택, 마이크와 캘리브레이션 선택, 낮은 레벨에서 시작, 스위트 스폿과 안내된 주변
측정 위치 캡처, 클리핑/저 SNR 오류에 즉시 대응, 타깃 커브 확인/편집, 계산, 예측되거나
보정된 응답 확인, 이름이 지정된 슬롯으로 내보내기, 프로젝트 저장이다. 현재 Dirac Live
문구는 예전의 "Volume Calibration" 단계를 "Measurement Levels"로, "Select Arrangement"를
"Select Sweet Spot"으로 부른다. 이 라벨들은 리서치 맥락이며 복사할 UI 에셋이 아니다.
이 제품에 유용한 상호작용 패턴은 다음과 같다:

1. 단계마다 하나의 주요 작업과 하나의 진행 동작만 보이게 유지한다.
2. 장치/경로 준비 상태를 측정 이전의 게이트로 만든다.
3. 마이크 측정 위치를 공간적으로 보여주고 측정 위치 전반의 진행 상황을 유지한다.
4. 단순한 타깃 컨트롤이 앞장서게 하고, 상세 컨트롤은 고급 화면 뒤에 둔다.
5. 계산과 배포를 분리하고 내보내기 전에 예측 응답을 보여준다.
6. 재측정 없이 타깃 선택을 다시 살펴볼 수 있을 만큼 프로젝트 상태를 저장한다.
7. 캘리브레이션으로 돌아올 때 레벨을 자동으로 낮추고, 안전 제한된 마스터 레벨을 올리기
   전에는 명시적인 동작을 요구한다.
8. 승인된 측정 위치를 자동 저장하고 계산을 장치 슬롯 내보내기와 분리해 유지한다.

EQforBeginner는 의도적으로 더 엄격한 안전 의미를 추가한다. 거부된 측정은 결코 조용히
평균에 포함되지 않고, 예측된 결과는 결코 검증된 것으로 간주되지 않으며, 최종 내보내기는
이후 단계에서 실제 48 kHz 필터 적용 재측정을 요구한다.

Dirac의 공개된 배치 안내는 중앙 스위트 스폿을 먼저 측정하고, 촘촘히 모인 점들이 아니라
실제 3차원 청취 공간을 사용하며, 마이크 방향을 그 캘리브레이션 파일에 맞출 것을 권장한다.
EQforBeginner는 그 원칙들을 자체 P0+5 배치에 적용하며, Dirac의 9/13/17 포인트 도해를
재현하지 않는다. Dirac 매뉴얼은 또한 검출된 임펄스 피크를 표시 시각 0 ms로 옮기는 것을
설명한다. EQforBeginner는 저장 데이터에 그 관례를 채택해서는 안 된다. 향후 어떤 표시
정렬이든 보존된 원래 타임라인 및 도달 시각과 분리된 상태로 유지되어야 한다.

출처:

- [Dirac Live public user manual](https://helpdesk.dirac.com/en/dirac-live/Dirac-Live-User-Manual-1eb2)
- [Dirac Live Quick Start](https://helpdesk.dirac.com/en/dirac-live/Dirac-Live-Quick-Start-Guide-fb62)
- [Dirac measurement order](https://helpdesk.dirac.com/en/dirac-live/In-what-order-should-I-measure-the-positions-cbe)
- [Dirac Live 3.13.4 terminology change](https://helpdesk.dirac.com/en/dirac-live/Dirac-Live-3134-LATEST-Software-Changelog-bfed)
- [Dirac Live Processor: where to start](https://helpdesk.dirac.com/en/dirac-art/Room-Correction-Suite-Where-do-I-start-2f39)
- [Dirac ART setup guide](https://helpdesk.dirac.com/en/dirac-art/Setup-Guide-c3cb)
- [Dirac Live Bass Control filter design](https://helpdesk.dirac.com/en/dirac-bass-control/Filter-Design-c592)
- [Dirac output-level safety lock](https://helpdesk.dirac.com/en/dirac-room-correction/Why-is-there-a-red-lock-on-the-Master-volume-in-the-Volume-Calibration-page-c178)
- [Dirac calibration-page automatic attenuation](https://helpdesk.dirac.com/en/dirac-room-correction/Why-does-the-volume-decrease-significantly-when-returning-to-the-Volume-Calibration-step-8319)

이 출처들은 사용 편의성에 대한 참고 자료일 뿐이다. 제품 문구는 "제한 대역 다중 측정 위치
룸 보정"과 "안내형 단일 서브우퍼 통합"이라고 말해야 하며, 기능 동등성을 주장해서는 안 된다.
현재 공개된 퀵스타트 프리셋은 이 프로젝트의 P0+5보다 더 많은 측정 위치를 사용한다.
P0+5 배치는 복사한 프리셋이 아니라 우리의 독자적인 프로토콜이다.
