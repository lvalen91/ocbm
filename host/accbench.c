// accbench.c — probe & benchmark the CCPA f_accessory bulk pipe from the Mac (USB host).
// Usage: accbench <info|read|write> [vidHex] [pidHex] [secs] [xfer_bytes]
//   info  : enumerate + dump device/interface/endpoint descriptors
//   read  : bulk-IN throughput  (adapter -> Mac; run `dd if=/dev/zero of=/dev/usb_accessory` on box)
//   write : bulk-OUT throughput (Mac -> adapter; run `dd if=/dev/usb_accessory of=/dev/null` on box)
#include <libusb-1.0/libusb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double now(void){ struct timespec ts; clock_gettime(CLOCK_MONOTONIC,&ts); return ts.tv_sec + ts.tv_nsec/1e9; }

int main(int argc, char** argv){
  unsigned vid = 0x0522, pid = 0;          // 1314 = 0x0522 ; pid 0 = any
  const char* mode = (argc>1)?argv[1]:"info";
  if(argc>2) vid=strtol(argv[2],0,16);
  if(argc>3) pid=strtol(argv[3],0,16);
  int secs = (argc>4)?atoi(argv[4]):5;
  int xfer = (argc>5)?atoi(argv[5]):65536;

  libusb_context* ctx=0;
  if(libusb_init(&ctx)){ printf("libusb_init failed\n"); return 1; }
  libusb_device** list; ssize_t n=libusb_get_device_list(ctx,&list);
  libusb_device* dev=0; struct libusb_device_descriptor dd;
  printf("-- scanning %zd USB devices --\n", n);
  for(ssize_t i=0;i<n;i++){
    if(libusb_get_device_descriptor(list[i],&dd)) continue;
    if(dd.idVendor==vid && (pid==0||dd.idProduct==pid)){ dev=list[i]; }
  }
  if(!dev){ printf("device VID %04x not found\n",vid); libusb_free_device_list(list,1); return 2; }
  libusb_get_device_descriptor(dev,&dd);
  printf("FOUND VID=%04x PID=%04x devClass=%d\n",dd.idVendor,dd.idProduct,dd.bDeviceClass);

  struct libusb_config_descriptor* cfg;
  if(libusb_get_active_config_descriptor(dev,&cfg)){ printf("no active config\n"); return 3; }
  int ifnum=-1, epin=-1, epout=-1, mpsin=0, mpsout=0;
  for(int i=0;i<cfg->bNumInterfaces;i++){
    const struct libusb_interface* itf=&cfg->interface[i];
    for(int a=0;a<itf->num_altsetting;a++){
      const struct libusb_interface_descriptor* id=&itf->altsetting[a];
      printf("IF %d alt %d class=%d sub=%d proto=%d neps=%d\n",
        id->bInterfaceNumber,id->bAlternateSetting,id->bInterfaceClass,id->bInterfaceSubClass,id->bInterfaceProtocol,id->bNumEndpoints);
      int bulkcount=0;
      for(int e=0;e<id->bNumEndpoints;e++){
        const struct libusb_endpoint_descriptor* ep=&id->endpoint[e];
        int isbulk=(ep->bmAttributes&3)==2;
        printf("  EP 0x%02x %s mps=%d\n",ep->bEndpointAddress, isbulk?"BULK":"(other)", ep->wMaxPacketSize);
        if(isbulk){ bulkcount++;
          if(ep->bEndpointAddress&0x80){ epin=ep->bEndpointAddress; mpsin=ep->wMaxPacketSize; }
          else { epout=ep->bEndpointAddress; mpsout=ep->wMaxPacketSize; }
        }
      }
      if(bulkcount>=2 && ifnum<0) ifnum=id->bInterfaceNumber;
    }
  }
  printf("=> chosen IF=%d bulkIN=0x%02x(mps %d) bulkOUT=0x%02x(mps %d)\n",ifnum,epin,mpsin,epout,mpsout);
  if(!strcmp(mode,"info")) return 0;
  if(ifnum<0){ printf("no bulk interface\n"); return 4; }

  libusb_device_handle* h;
  if(libusb_open(dev,&h)){ printf("open failed (perm?)\n"); return 5; }
  libusb_set_auto_detach_kernel_driver(h,1);
  int rc=libusb_claim_interface(h,ifnum);
  if(rc){ printf("claim IF %d failed rc=%d (%s)\n",ifnum,rc,libusb_error_name(rc)); return 6; }

  unsigned char* buf=malloc(xfer); memset(buf,0xA5,xfer);
  int ep = !strcmp(mode,"read")?epin:epout;
  double t0=now(), tend=t0+secs; long long total=0; int done;
  while(now()<tend){
    rc=libusb_bulk_transfer(h,ep,buf,xfer,&done,1000);
    if(rc==0 || rc==LIBUSB_ERROR_TIMEOUT){ total+=done; if(rc==LIBUSB_ERROR_TIMEOUT && done==0) { /* idle */ } }
    else { printf("bulk rc=%d (%s) after %lld bytes\n",rc,libusb_error_name(rc),total); break; }
  }
  double dt=now()-t0;
  printf("RESULT mode=%s xfer=%d bytes=%lld secs=%.2f => %.2f MB/s (%.1f Mbps)\n",
    mode,xfer,total,dt, total/1e6/dt, total*8.0/1e6/dt);
  libusb_release_interface(h,ifnum);
  libusb_close(h);
  libusb_free_device_list(list,1);
  libusb_exit(ctx);
  return 0;
}
