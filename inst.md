Quiero hacer con rust hacer la unificacion dentro de las carpetas a los pdfs, la carpeta no se topa ni los pdfs no se pueden mover del sitio original donde se encontraban, la idea es que los unicos nombres validos para que esto funcione son:


PI.pdf
CC.pdf
CV.pdf
AES.pdf
053.pdf
006.pdf
007.pdf
017.pdf
018.pdf
018A.pdf
113.pdf
114.pdf
115.pdf
ORS.pdf
002.pdf
010A.pdf
010B.pdf
012A.pdf
012B.pdf
033.pdf
013A.pdf
013B.pdf
PTR.pdf
RTR.pdf
08.pdf
FSCS.pdf
FSICS.pdf
FRDCS.pdf
ANX2.pdf
HR.pdf
RHD.pdf
IMT.pdf
CEC.pdf
RAD.pdf
ITS.pdf
RVD.pdf
119.pdf


la idea es que por ejemplo se tiene la carpeta

5555555 dentro de la carpeta esta asi

5555555
	PI_01.pdf
	PI_2.pdf
	PI.pdf
	CEC.pdf
	RAD.pdf


la idea es que aquellos que estan bien nombrados se mantengan asi por ejemplo 	RAD.pdf ya no hace falta toparlo pero 	PI_01.pdf, 	PI_2.pdf y 	PI.pdf se deberia hacer un proceso para renombrarlos a PI_01_aux.pdf , PI_2_aux.pdf y PI_aux.pdf se deberia revisar el contenido y unir estos auxiliares que en este caso son 3 pero puden ser mas o menos pdfs con nombres similares unirlos pero revisar es decir si es posible leer el contenido y verificar el tamaño individual y unido para saber que se hizo bien la operacion hagamos originalmente un script para probar en local


