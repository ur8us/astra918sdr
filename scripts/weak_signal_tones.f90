! Test-only driver for an external, unmodified WSJT-X source checkout.
program tones
  implicit none
  character(37) :: message = 'CQ K1ABC FN42', sent
  character(8) :: mode
  integer :: i3, n3, ft8(79), ft4(103)
  integer(kind=1) :: bits(77)
  call get_command_argument(1, mode)
  if (trim(mode) == 'FT8') then
    call genft8(message, i3, n3, sent, bits, ft8)
    write(*,'(79i1)') ft8
  else
    call genft4(message, 0, sent, bits, ft4)
    write(*,'(103i1)') ft4
  end if
end program
