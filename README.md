# EQforBeginner

앱에서 클릭 몇 번과 측정만으로 자동 스피커 보정을 해주는 앱을 만들었습니다.
완전 초보자를 대상으로 REW 조차도 사용하기 번거롭고 귀찮다 하시는 분들께 추천하나 정교한 보정을 제공하기 위해 만들었습니다.
(현재 베타버전에 Claude가 설계한 코드와 알고리즘으로 버그와 알고리즘/방법론상 결함이 있을 수 있습니다. **버그, 작동 안됨, 방법론적 결함 등은 디시 댓글에 알려주시면 감사하겠습니다.**)

**알고리즘 및 방법론은 최상위 디렉토리의 md 파일과 docs 디렉토리의 md파일에 있습니다. 피드백 적극 환영합니다!**

**SECS 알고리즘을 구현해주신 한플님 감사합니다."**
https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1

현재 버전 (v0.1.2)

- **서브우퍼 통합 측정을 스윕 3회로 단축** (광대역 방식 기본값): 크로스오버를 최대로
  올린 서브 1회 + 풀레인지 메인 2회를 재고, 후보 크로스오버 상태는 선언한 슬로프
  모델로 합성한다. 기존 실측 방식(후보마다 3회)도 그대로 선택할 수 있다.
- **서브 레벨 권장값 추가**: 크로스오버·지연·극성과 함께 "서브 레벨을 몇 dB 바꿀지"를
  같이 알려준다. 채점을 실제 배포 레벨에서 하도록 바꾸면서, 서브가 메인보다 크게 잡힌
  측정에서 추천 크로스오버가 후보 목록의 최고값을 따라가던 문제를 해결했다.
- 추천 메인 지연이 음수면 서브 출력에 지연을 거는 방법으로 안내한다.
- 결과 그래프: 혼합위상 보정 곡선을 Raw 레벨에 맞춰 그려 모양 비교가 쉽고(정렬량은
  그래프 아래에 표기), 20–500 Hz 보기가 전 대역 보기와 같아지던 문제를 고쳤다.

이전 버전 (v0.1.1)

- **SECS 알고리즘 추가**
- 2.1채널 선택 시 그에 맞게 SECS 알고리즘 개량
- 기존 6지점 측정에서 9지점 측정으로 변경

## Usage

- [다운로드 링크 Windows](https://drive.google.com/file/d/1Zs7bjpKbQbzAX1rT5_GdKL2CbDrJgRPD/view?usp=sharing)
- [다운로드 링크 Mac](https://drive.google.com/file/d/1FDX1ztoL5BwTNrM_v8vBD-Fnb7vpFHU0/view?usp=sharing)

- [L 스윕 다운로드 링크](https://drive.google.com/file/d/1oUExSKQPChtX7unysMLhCc10M9hqM8DW/view?usp=sharing)
- [R 스윕 다운로드 링크](https://drive.google.com/file/d/1FIxWwctuwBQ99odKo2pj-PBXEp3CO1Ce/view?usp=sharing)

### 0 - 측정 전 준비물

준비물: 스피커(+서브우퍼), miniDSP UMIK-1 측정 마이크, Convolution 필터 적용 가능 프로그램
있으면 좋은것: 마이크 거치대, Roon, 귀마개(스윕 소리가 상당히 시끄러움)

### 1 - 측정 준비

마이크를 청취 위치에 고정해주세요

### 2 - 캘리브레이션 파일 불러오기

<img width="1438" height="806" alt="Image" src="https://github.com/user-attachments/assets/2f1b7102-715e-44f5-93db-ff0a234e6ced" />

마이크와 캘리브레이션 파일을 불러옵니다.

UMIK-1 사용 시 마이크 채널 2개가 잡히면 1번 채널을 선택하시면 됩니다.

miniDSP에서 UMIK-1 마이크 **90도** 캘리브레이션 파일을 받아 올려주세요.

위에서 다운받은 L 스윕 파일, R 스윕 파일을 올려주세요.

### 3 - 서브우퍼 위상, 크로스오버, 지연 조정

**서브우퍼를 안쓰시는 분은 4로 넘어가면 됩니다.**

<img width="1428" height="789" alt="Image" src="https://github.com/user-attachments/assets/746e2057-f37f-4163-a197-c58217670afd" />

기본값인 **광대역 방식**은 측정 3번이면 됩니다. ① 앰프에서 서브 출력을 끄고(스피커가 풀레인지가 됩니다) L, R을 각각 측정 → ② 서브 출력을 켜고 크로스오버를 최대(예: WiiM 250Hz)로 돌린 뒤 Sub only를 측정합니다. 앱이 입력한 후보 크로스오버들(기본 40~120Hz)을 선택한 필터 슬로프 모델(기본 LR4, WiiM 등 대부분 기기)로 시뮬레이션해 최적점을 찾습니다. 세 측정 모두 볼륨을 동일하게 유지해주세요.

기기의 크로스오버 슬로프를 모르거나 특이한 경우에는 **실측 방식**을 선택해 이전처럼 크로스오버(예: 70, 80, 90Hz)마다 L main only, R main only, Sub only를 각각 측정할 수 있습니다.

스윕 시작/종료 신호는 항상 R 스피커에서 나오니 Sub only 측정 시 L 스피커를 뮤트하거나 물리적으로 앰프에서 케이블을 분리하고 L 스윕을 재생해주세요.

**주의** 물리적으로 앰프에서 L 스피커 케이블 분리 시 반드시 **앰프 전원을 끄고** 진행해주세요.

**진공관 앰프 사용 시 L 스피커 분리 후 L 스윕 자체가 위험할 수 있으니 L 스피커를 소프트웨어적으로 뮤트해주세요**

<img width="1438" height="800" alt="Image" src="https://github.com/user-attachments/assets/15044f4a-f518-446e-884c-27f93d5867c9" />

**측정 대기** 버튼을 누르고 스윕을 재생해주세요. 자동으로 스윕 시작/종료 신호를 인식합니다.

이렇게 최적 서브우퍼 위상, 크로스오버, 지연을 찾았습니다.

### 4 - 다지점 측정

<img width="1433" height="797" alt="Image" src="https://github.com/user-attachments/assets/633e3357-60b0-4b72-b010-b63ff5ba98c7" />

(서브우퍼가 있다면 서브우퍼 전원을 켜고) 같은 스윕을 재생해서 앱 안내대로 측정 위치를 바꿔가며 L(+Sub), R(+Sub) 다지점 측정을 해줍니다.

<img width="1433" height="802" alt="Image" src="https://github.com/user-attachments/assets/b3edf5fd-a6e2-46db-9482-d93a03cf245b" />

### 5 - 보정값 생성

이제 보정이 의도한 대로 되었나 검증 단계입니다. zip 파일을 다운받고 Roon Convolution 필터를 적용해주세요.

.wav(또는 .zip)형식 convolution 파일을 쓰지 않는다면 다운받을 파일을 ChatGPT 등에 올리고 PEQ로 최대한 근사시켜달라고 하면 됩니다. 그리고 보정값을 본인이 사용하는 EQ 시스템에 입력해주세요. (SECS 알고리즘 사용 시 PEQ만으로는 부족합니다. 이때는 Convolution 파일을 적용해주세요)

<img width="1437" height="802" alt="Image" src="https://github.com/user-attachments/assets/ffb60ce5-1b16-4a90-a09d-caddee7cfeb6" />

**고급 옵션** 버튼을 누르면 SECS 알고리즘 적용 가능합니다. UI가 잘 안보일 수 있으니 주의해주세요

### 6 - 검증 단계

마지막으로 청취점 중앙에서 측정으로 보정이 올바르게 되었나 검증해주세요. (SECS 알고리즘 사용 시 검증 건너뛰기도 가능)

### 7 - 완료

이제 보정이 완료되었습니다. 최종적으로 생성된 Convolution 파일을 사용하시면 됩니다.
검증 통과가 되지 않는다면 앱의 안내대로 다시 진행해주세요.

## 개발자를 위한 추가 설명

- 앱은 측정값 캐시 기능이 있습니다. 앱에서 측정된 .wav 파일을 REW에서 불러와 확인할 수 있습니다.

## 크레딧 / 출처

이 앱의 **SECS 고급 옵션**은 **한플**님이 만드신 SECS 룸 보정 프로그램을 이식한 것입니다.
원작자께서 **MIT 라이선스**로 사용을 허가해 주셨습니다. 감사합니다.

- 원작자: 한플
- 원본: [디시인사이드 스피커 갤러리 SECS 원본 글](https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1)

다지점 평균, 타겟 커브 추종, 2.1 공유 서브 대역 공통화, 폐루프 검증과 내보내기는
이 프로젝트에서 추가한 것이라 원작자의 설계가 아닙니다. 해당 부분의 결함은 이
프로젝트 책임입니다. 라이선스 전문과 이식 범위는
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)에 있습니다.
