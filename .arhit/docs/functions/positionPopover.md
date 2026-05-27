# positionPopover

Позиционирует shared попап у черты коммита (gantt.js). position:fixed; снимает getBoundingClientRect черты и попапа; по горизонтали справа от черты (anchor.right+GAP), при нехватке места слева, затем кламп в [MARGIN, vw-width-MARGIN]; по вертикали выравнивание по верху черты с клампом в вьюпорт. Не вылезает за window.innerWidth/innerHeight.
