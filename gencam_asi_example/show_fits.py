#!/usr/bin/env python
import astropy.io.fits as pf
import numpy as np
import matplotlib.pyplot as plt
import sys

def show_fits(fitsfile):
    hdu = pf.open(fitsfile)
    if 'COMPRESSED_IMAGE' in hdu[0].header:
        data = hdu[1].data.astype(float)
        keys = hdu[1].header.keys()
        for key in keys:
            print(f'{key}: {hdu[1].header[key]}')
    else:
        data = hdu[0].data.astype(float)
        keys = hdu[0].header.keys()
        for key in keys:
            print(f'{key}: {hdu[0].header[key]}')
    vmin = np.percentile(data, 1)
    vmax = np.percentile(data, 99)
    plt.title('Width: %d, Height: %d' % (data.shape[1], data.shape[0]))
    plt.imshow(data, cmap='gray', vmin=vmin, vmax=vmax, aspect='auto')
    plt.colorbar()
    plt.show()

if __name__ == '__main__':
    show_fits(sys.argv[1])